//! End-to-end for the MySQL frontend, driven by a minimal raw-protocol client
//! written here (no heavy client dependency). It performs the handshake, sends
//! COM_QUERY, and parses the text-protocol result set.

use etherealdb::config::{Config, CrushConfig};
use etherealdb::server::{self, Proto, Shared};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn start_mysql(cfg: Config) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared = Shared::new(cfg);
    tokio::spawn(server::run(listener, shared, Proto::Mysql));
    port
}

struct MyClient {
    stream: TcpStream,
}

impl MyClient {
    async fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut c = MyClient { stream };
        // Read the server handshake (discard the contents).
        c.read_packet().await;
        // Send a minimal handshake response: PROTOCOL_41 | SECURE | PLUGIN_AUTH.
        let mut p = Vec::new();
        let caps: u32 = 0x0200 | 0x8000 | 0x0008_0000;
        p.extend_from_slice(&caps.to_le_bytes());
        p.extend_from_slice(&(16u32 * 1024 * 1024).to_le_bytes()); // max packet
        p.push(0x21); // charset
        p.extend_from_slice(&[0u8; 23]); // reserved
        p.extend_from_slice(b"root\0"); // username
        p.push(0); // auth-response length = 0 (no password)
        p.extend_from_slice(b"mysql_native_password\0");
        c.write_packet(&p, 1).await;
        // Expect an OK packet (first byte 0x00).
        let ok = c.read_packet().await;
        assert_eq!(ok.first(), Some(&0x00), "expected auth OK");
        c
    }

    async fn write_packet(&mut self, payload: &[u8], seq: u8) {
        let mut header = [0u8; 4];
        header[0..3].copy_from_slice(&(payload.len() as u32).to_le_bytes()[0..3]);
        header[3] = seq;
        self.stream.write_all(&header).await.unwrap();
        self.stream.write_all(payload).await.unwrap();
    }

    async fn read_packet(&mut self) -> Vec<u8> {
        let mut header = [0u8; 4];
        self.stream.read_exact(&mut header).await.unwrap();
        let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).await.unwrap();
        payload
    }

    /// Run a query and return (column_names, rows-of-string-values).
    async fn query(&mut self, sql: &str) -> (Vec<String>, Vec<Vec<String>>) {
        let mut payload = vec![0x03u8]; // COM_QUERY
        payload.extend_from_slice(sql.as_bytes());
        self.write_packet(&payload, 0).await;

        let first = self.read_packet().await;
        // OK packet (non-result) -> no columns/rows.
        if first.first() == Some(&0x00) {
            return (vec![], vec![]);
        }
        assert_ne!(first.first(), Some(&0xff), "got error packet: {first:?}");

        let mut idx = 0;
        let col_count = read_lenenc_int(&first, &mut idx) as usize;

        // Column definitions.
        let mut names = Vec::new();
        for _ in 0..col_count {
            let def = self.read_packet().await;
            names.push(parse_column_name(&def));
        }
        // EOF after column defs.
        let eof = self.read_packet().await;
        assert!(is_eof(&eof), "expected EOF after columns");

        // Rows until EOF.
        let mut rows = Vec::new();
        loop {
            let pkt = self.read_packet().await;
            if is_eof(&pkt) {
                break;
            }
            let mut i = 0;
            let mut row = Vec::new();
            for _ in 0..col_count {
                row.push(read_lenenc_str(&pkt, &mut i));
            }
            rows.push(row);
        }
        (names, rows)
    }
}

fn is_eof(p: &[u8]) -> bool {
    p.first() == Some(&0xfe) && p.len() < 9
}

fn read_lenenc_int(p: &[u8], i: &mut usize) -> u64 {
    let first = p[*i];
    *i += 1;
    match first {
        0xfc => {
            let v = u16::from_le_bytes([p[*i], p[*i + 1]]) as u64;
            *i += 2;
            v
        }
        0xfd => {
            let v = u32::from_le_bytes([p[*i], p[*i + 1], p[*i + 2], 0]) as u64;
            *i += 3;
            v
        }
        0xfe => {
            let mut a = [0u8; 8];
            a.copy_from_slice(&p[*i..*i + 8]);
            *i += 8;
            u64::from_le_bytes(a)
        }
        n => n as u64,
    }
}

fn read_lenenc_str(p: &[u8], i: &mut usize) -> String {
    let len = read_lenenc_int(p, i) as usize;
    let s = String::from_utf8_lossy(&p[*i..*i + len]).into_owned();
    *i += len;
    s
}

fn parse_column_name(def: &[u8]) -> String {
    // def, schema, table, org_table, name, ... — skip the first four lenenc strs.
    let mut i = 0;
    for _ in 0..4 {
        let _ = read_lenenc_str(def, &mut i);
    }
    read_lenenc_str(def, &mut i)
}

fn seeded() -> Config {
    Config { seed: Some(7), ..Config::default() }
}

#[tokio::test]
async fn handshake_and_select() {
    let port = start_mysql(seeded()).await;
    let mut c = MyClient::connect(port).await;

    let (names, rows) = c.query("select id, email, is_active from users limit 5").await;
    assert_eq!(names, ["id", "email", "is_active"]);
    assert!((1..=5).contains(&rows.len()), "got {} rows", rows.len());
    for row in &rows {
        assert!(row[0].parse::<i64>().is_ok(), "id not int: {}", row[0]);
        assert!(row[1].contains('@'), "email not email: {}", row[1]);
        assert!(row[2] == "1" || row[2] == "0", "bool not 1/0: {}", row[2]);
    }
}

#[tokio::test]
async fn count_returns_one_row() {
    let port = start_mysql(Config::default()).await;
    let mut c = MyClient::connect(port).await;
    let (_n, rows) = c.query("select count(*) from orders").await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0][0].parse::<i64>().is_ok());
}

#[tokio::test]
async fn dml_returns_no_resultset() {
    let port = start_mysql(Config::default()).await;
    let mut c = MyClient::connect(port).await;
    let (names, rows) = c.query("insert into t (a) values (1)").await;
    assert!(names.is_empty() && rows.is_empty());
}

#[tokio::test]
async fn catalog_query_is_empty() {
    let port = start_mysql(Config::default()).await;
    let mut c = MyClient::connect(port).await;
    // information_schema is a system catalog -> zero rows.
    let (_n, rows) = c.query("select table_name from information_schema.tables").await;
    assert!(rows.is_empty(), "catalog should be empty, got {}", rows.len());
}

#[tokio::test]
async fn crush_streams_many_rows() {
    let cfg = Config {
        crush: CrushConfig { enabled: true, max_rows: 8_000, ..CrushConfig::default() },
        ..Config::default()
    };
    let port = start_mysql(cfg).await;
    let mut c = MyClient::connect(port).await;

    let (_n, rows) = c.query("select * from users").await;
    assert_eq!(rows.len(), 8_000, "crush should stream max_rows");
}

#[tokio::test]
async fn many_types_render_as_text() {
    let port = start_mysql(seeded()).await;
    let mut c = MyClient::connect(port).await;
    let (names, rows) = c
        .query("select account_uuid, signup_date, balance, created_at from members limit 3")
        .await;
    assert_eq!(names, ["account_uuid", "signup_date", "balance", "created_at"]);
    for row in &rows {
        assert_eq!(row[0].len(), 36, "uuid text length");
        assert_eq!(row[1].len(), 10, "date YYYY-MM-DD");
        assert!(row[2].parse::<f64>().is_ok(), "balance not decimal: {}", row[2]);
        assert_eq!(row[3].len(), 19, "datetime length");
    }
}
