//! MySQL wire protocol frontend (protocol 10, text result sets).
//!
//! Supported: initial handshake + trust auth (advertised as
//! `mysql_native_password`, but any credentials are accepted), `COM_QUERY`
//! with text-protocol result sets, and the housekeeping commands
//! (`COM_PING`, `COM_INIT_DB`, `COM_QUIT`). Prepared statements
//! (`COM_STMT_PREPARE`) are declined with an error so drivers fall back to the
//! text protocol. Everything downstream — shape, inference, generation, crush,
//! catalog stubs — is shared with the Postgres frontend via `shape`.

use std::sync::Arc;

use bytes::{BufMut, BytesMut};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::config::{Config, fnv64};
use crate::generate::{Gen, gen_value};
use crate::infer::WireType;
use crate::server::Shared;
use crate::shape::{self, CrushClass, Resolved, ResultShape, StmtKind};

// --- capability flags (server advertises a useful subset) ---
const CLIENT_LONG_PASSWORD: u32 = 0x0000_0001;
const CLIENT_CONNECT_WITH_DB: u32 = 0x0000_0008;
const CLIENT_PROTOCOL_41: u32 = 0x0000_0200;
const CLIENT_SECURE_CONNECTION: u32 = 0x0000_8000;
const CLIENT_PLUGIN_AUTH: u32 = 0x0008_0000;

const SERVER_CAPS: u32 = CLIENT_LONG_PASSWORD
    | CLIENT_CONNECT_WITH_DB
    | CLIENT_PROTOCOL_41
    | CLIENT_SECURE_CONNECTION
    | CLIENT_PLUGIN_AUTH;

const SERVER_STATUS_AUTOCOMMIT: u16 = 0x0002;
const CHARSET_UTF8MB4: u8 = 0xff; // utf8mb4_0900_ai_ci, close enough
const MAX_PACKET: usize = 64 * 1024 * 1024;

// --- commands ---
const COM_QUIT: u8 = 0x01;
const COM_INIT_DB: u8 = 0x02;
const COM_QUERY: u8 = 0x03;
const COM_FIELD_LIST: u8 = 0x04;
const COM_PING: u8 = 0x0e;
const COM_STMT_PREPARE: u8 = 0x16;

// --- MySQL column type codes ---
mod coltype {
    pub const LONG: u8 = 0x03;
    pub const DOUBLE: u8 = 0x05;
    pub const TIMESTAMP: u8 = 0x07;
    pub const LONGLONG: u8 = 0x08;
    pub const DATE: u8 = 0x0a;
    pub const TIME: u8 = 0x0b;
    pub const DATETIME: u8 = 0x0c;
    pub const TINY: u8 = 0x01;
    pub const JSON: u8 = 0xf5;
    pub const NEWDECIMAL: u8 = 0xf6;
    pub const VAR_STRING: u8 = 0xfd;
}

const CRUSH_CHUNK: u64 = 1000;

static WIDE_CRUSH_COLUMNS: &[&str] = &[
    "id",
    "uuid",
    "name",
    "email",
    "phone",
    "status",
    "created_at",
    "updated_at",
    "price",
    "is_active",
    "description",
    "metadata",
];

fn mysql_type(wt: WireType) -> u8 {
    match wt {
        WireType::Bool => coltype::TINY,
        WireType::Int4 => coltype::LONG,
        WireType::Int8 => coltype::LONGLONG,
        WireType::Float8 => coltype::DOUBLE,
        WireType::Numeric => coltype::NEWDECIMAL,
        WireType::Text | WireType::Uuid => coltype::VAR_STRING,
        WireType::Date => coltype::DATE,
        WireType::Time => coltype::TIME,
        WireType::Timestamp => {
            // DATETIME and TIMESTAMP render the same way over text; either works.
            let _ = coltype::TIMESTAMP;
            coltype::DATETIME
        }
        WireType::Json => coltype::JSON,
    }
}

/// Render a generated value in MySQL's expected text form. Only booleans differ
/// from the Postgres text we generate: MySQL wants `1`/`0`, not `t`/`f`.
fn mysql_value(c: &Resolved, rng: &mut impl Rng, g: Gen) -> String {
    if let Some(lit) = &c.literal {
        return lit.clone();
    }
    let v = gen_value(c.st, c.wt, rng, g);
    if c.wt == WireType::Bool {
        if v == "t" { "1".into() } else { "0".into() }
    } else {
        v
    }
}

// --- length-encoded integers/strings ---

fn put_lenenc_int(buf: &mut BytesMut, n: u64) {
    match n {
        0..=0xfa => buf.put_u8(n as u8),
        0xfb..=0xffff => {
            buf.put_u8(0xfc);
            buf.put_u16_le(n as u16);
        }
        0x1_0000..=0xff_ffff => {
            buf.put_u8(0xfd);
            buf.put_uint_le(n, 3);
        }
        _ => {
            buf.put_u8(0xfe);
            buf.put_u64_le(n);
        }
    }
}

fn put_lenenc_str(buf: &mut BytesMut, s: &str) {
    put_lenenc_int(buf, s.len() as u64);
    buf.put_slice(s.as_bytes());
}

/// Writes packets with the running sequence id the protocol requires. The id
/// resets to 0 for each new command from the client; server replies count up.
struct Conn {
    stream: TcpStream,
    seq: u8,
}

impl Conn {
    /// Frame `payload` as a packet (3-byte length + 1-byte seq) and send it.
    async fn write_packet(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let mut header = [0u8; 4];
        let len = payload.len() as u32;
        header[0..3].copy_from_slice(&len.to_le_bytes()[0..3]);
        header[3] = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.stream.write_all(&header).await?;
        self.stream.write_all(payload).await
    }

    /// Read one packet's payload (sets `seq` to one past what the client sent).
    async fn read_packet(&mut self) -> std::io::Result<Vec<u8>> {
        let mut header = [0u8; 4];
        self.stream.read_exact(&mut header).await?;
        let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        self.seq = header[3].wrapping_add(1);
        if len > MAX_PACKET {
            return Err(std::io::Error::other("packet too large"));
        }
        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).await?;
        Ok(payload)
    }
}

fn ok_packet(affected: u64, last_insert_id: u64) -> BytesMut {
    let mut b = BytesMut::new();
    b.put_u8(0x00); // OK header
    put_lenenc_int(&mut b, affected);
    put_lenenc_int(&mut b, last_insert_id);
    b.put_u16_le(SERVER_STATUS_AUTOCOMMIT);
    b.put_u16_le(0); // warnings
    b
}

fn eof_packet() -> BytesMut {
    let mut b = BytesMut::new();
    b.put_u8(0xfe); // EOF header
    b.put_u16_le(0); // warnings
    b.put_u16_le(SERVER_STATUS_AUTOCOMMIT);
    b
}

fn err_packet(code: u16, sql_state: &str, msg: &str) -> BytesMut {
    let mut b = BytesMut::new();
    b.put_u8(0xff);
    b.put_u16_le(code);
    b.put_u8(b'#');
    b.put_slice(sql_state.as_bytes());
    b.put_slice(msg.as_bytes());
    b
}

fn column_def(c: &Resolved) -> BytesMut {
    let mut b = BytesMut::new();
    put_lenenc_str(&mut b, "def"); // catalog
    put_lenenc_str(&mut b, "ethereal"); // schema
    put_lenenc_str(&mut b, ""); // table
    put_lenenc_str(&mut b, ""); // org_table
    put_lenenc_str(&mut b, &c.name); // name
    put_lenenc_str(&mut b, &c.name); // org_name
    put_lenenc_int(&mut b, 0x0c); // length of fixed-length fields
    let numeric = matches!(
        c.wt,
        WireType::Int4 | WireType::Int8 | WireType::Float8 | WireType::Numeric | WireType::Bool
    );
    b.put_u16_le(if numeric { 0x3f } else { 0x21 }); // charset: binary vs utf8
    b.put_u32_le(if numeric { 20 } else { 1024 }); // column length (display)
    b.put_u8(mysql_type(c.wt));
    b.put_u16_le(0); // flags
    b.put_u8(0); // decimals
    b.put_u16_le(0); // filler
    b
}

fn text_row(cols: &[Resolved], rng: &mut impl Rng, g: Gen) -> BytesMut {
    let mut b = BytesMut::new();
    for c in cols {
        put_lenenc_str(&mut b, &mysql_value(c, rng, g));
    }
    b
}

pub async fn handle(stream: TcpStream, shared: Arc<Shared>) -> std::io::Result<()> {
    let peer = stream.peer_addr()?;
    let mut conn = Conn { stream, seq: 0 };

    // --- handshake: send greeting, accept whatever the client answers ---
    send_handshake(&mut conn).await?;
    let response = conn.read_packet().await?;
    let (user, database) = parse_handshake_response(&response);
    info!(%peer, user, database, "mysql session open (every database exists here)");
    conn.write_packet(&ok_packet(0, 0)).await?; // auth OK, trust everyone

    // --- command phase ---
    loop {
        conn.seq = 0; // each command starts a fresh sequence
        let packet = match conn.read_packet().await {
            Ok(p) => p,
            Err(_) => return Ok(()), // client hung up
        };
        let Some((&cmd, rest)) = packet.split_first() else {
            continue;
        };
        match cmd {
            COM_QUIT => return Ok(()),
            COM_PING => conn.write_packet(&ok_packet(0, 0)).await?,
            COM_INIT_DB => conn.write_packet(&ok_packet(0, 0)).await?,
            COM_FIELD_LIST => conn.write_packet(&eof_packet()).await?,
            COM_QUERY => {
                let sql = String::from_utf8_lossy(rest).into_owned();
                debug!(%peer, %sql, "mysql query");
                // --- ghosts: haunt the query
                let ghosts = &shared.cfg.ghosts;
                if ghosts.haunting() {
                    ghosts.maybe_latency().await;
                    if ghosts.maybe_drop() {
                        debug!(%peer, "ghost: dropping connection");
                        return Ok(());
                    }
                    if ghosts.maybe_error() {
                        debug!(%peer, "ghost: injecting error");
                        conn.write_packet(&err_packet(1105, "HY000", "a ghost ate your query"))
                            .await?;
                        continue;
                    }
                }
                run_query(&mut conn, &sql, &shared, peer).await?;
            }
            COM_STMT_PREPARE => {
                conn.write_packet(&err_packet(
                    1295,
                    "HY000",
                    "EtherealDB speaks the MySQL text protocol only; disable prepared statements (e.g. interpolateParams=true)",
                ))
                .await?;
            }
            other => {
                debug!(%peer, cmd = other, "mysql: unsupported command, acking");
                conn.write_packet(&ok_packet(0, 0)).await?;
            }
        }
    }
}

async fn send_handshake(conn: &mut Conn) -> std::io::Result<()> {
    let mut b = BytesMut::new();
    b.put_u8(10); // protocol version
    b.put_slice(b"8.0.36-EtherealDB\0"); // server version
    b.put_u32_le(0x0000_2a2a); // connection id
    // auth-plugin-data-part-1 (8 bytes of scramble) + filler
    let scramble: [u8; 20] = rand::random();
    b.put_slice(&scramble[..8]);
    b.put_u8(0); // filler
    b.put_u16_le((SERVER_CAPS & 0xffff) as u16); // capability flags (lower)
    b.put_u8(CHARSET_UTF8MB4);
    b.put_u16_le(SERVER_STATUS_AUTOCOMMIT);
    b.put_u16_le((SERVER_CAPS >> 16) as u16); // capability flags (upper)
    b.put_u8(21); // length of auth plugin data
    b.put_slice(&[0u8; 10]); // reserved
    b.put_slice(&scramble[8..20]); // auth-plugin-data-part-2 (12 bytes)
    b.put_u8(0); // NUL terminator for part-2
    b.put_slice(b"mysql_native_password\0");
    conn.write_packet(&b).await
}

/// Pull the username and (optional) default database out of the client's
/// handshake response. We don't validate the auth token — trust auth.
fn parse_handshake_response(p: &[u8]) -> (String, String) {
    // client_flag(4) max_packet(4) charset(1) reserved(23) then username (cstr)
    if p.len() < 32 {
        return ("ghost".into(), String::new());
    }
    let client_flags = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
    let mut i = 32;
    let user = read_cstr(p, &mut i);
    // auth response: lenenc (if CLIENT_PLUGIN_AUTH_LENENC) or 1-byte length
    let auth_len = p.get(i).copied().unwrap_or(0) as usize;
    i += 1 + auth_len;
    let mut database = String::new();
    if client_flags & CLIENT_CONNECT_WITH_DB != 0 {
        database = read_cstr(p, &mut i);
    }
    let user = if user.is_empty() {
        "ghost".into()
    } else {
        user
    };
    (user, database)
}

fn read_cstr(p: &[u8], i: &mut usize) -> String {
    let start = (*i).min(p.len());
    let end = p[start..]
        .iter()
        .position(|&c| c == 0)
        .map_or(p.len(), |n| start + n);
    let s = String::from_utf8_lossy(&p[start..end]).into_owned();
    *i = (end + 1).min(p.len() + 1);
    s
}

fn seed_rng(cfg: &Config, sql: &str) -> ChaCha8Rng {
    match cfg.seed {
        Some(seed) => ChaCha8Rng::seed_from_u64(seed ^ fnv64(sql.as_bytes())),
        None => ChaCha8Rng::from_os_rng(),
    }
}

async fn run_query(
    conn: &mut Conn,
    sql: &str,
    shared: &Shared,
    peer: std::net::SocketAddr,
) -> std::io::Result<()> {
    let cfg = &shared.cfg;
    // MySQL clients send several statements in one COM_QUERY only with
    // CLIENT_MULTI_STATEMENTS (which we don't advertise); take the first.
    let stmt = shape::split_statements(sql)
        .into_iter()
        .next()
        .unwrap_or("");
    let shape = shape::extract(stmt);

    match &shape.kind {
        StmtKind::Empty => conn.write_packet(&ok_packet(0, 0)).await,
        StmtKind::Select => select_response(conn, stmt, &shape, shared, peer).await,
        StmtKind::Insert => conn.write_packet(&ok_packet(1, rand_id(cfg, stmt))).await,
        StmtKind::Update | StmtKind::Delete => {
            let mut rng = seed_rng(cfg, stmt);
            conn.write_packet(&ok_packet(rng.random_range(0..50), 0))
                .await
        }
        // BEGIN/COMMIT/SET/CREATE/... — MySQL just wants an OK.
        StmtKind::Command(_) => conn.write_packet(&ok_packet(0, 0)).await,
    }
}

fn rand_id(cfg: &Config, sql: &str) -> u64 {
    let mut rng = seed_rng(cfg, sql);
    rng.random_range(1..1_000_000)
}

async fn select_response(
    conn: &mut Conn,
    sql: &str,
    shape: &ResultShape,
    shared: &Shared,
    peer: std::net::SocketAddr,
) -> std::io::Result<()> {
    let cfg = &shared.cfg;
    let class = if cfg.crush.enabled {
        shape.crush_class(cfg.crush.threshold)
    } else {
        CrushClass::Safe
    };

    // Crush: stream an avalanche of rows.
    if matches!(class, CrushClass::Crush { .. }) && !cfg.crush.warn_only {
        if let Ok(_permit) = shared.crush_slots.try_acquire() {
            let cols: Vec<Resolved> = if shape.select_star {
                WIDE_CRUSH_COLUMNS
                    .iter()
                    .map(|n| Resolved::from_name(n, &cfg.rules))
                    .collect()
            } else {
                resolve_cols(shape, cfg)
            };
            warn!(%peer, reasons = class.reasons(), "CRUSH (mysql)");
            return crush_stream(conn, &cols, cfg, sql, peer).await;
        }
    } else if let CrushClass::Crush { .. } = class {
        warn!(%peer, reasons = class.reasons(), "unsafe query (warn-only)");
    }

    // Normal result set.
    let cols = resolve_cols(shape, cfg);
    let mut rng = seed_rng(cfg, sql);
    let mut n = if shape.force_empty {
        0
    } else if shape.aggregate_only() || shape.table_hint.is_none() {
        1
    } else {
        rng.random_range(cfg.rows_min..=cfg.rows_max.max(cfg.rows_min))
    };
    if let Some(limit) = shape.limit {
        n = n.min(limit);
    }

    send_result_header(conn, &cols).await?;
    for _ in 0..n {
        let row = text_row(&cols, &mut rng, cfg.gen_ctx());
        conn.write_packet(&row).await?;
    }
    conn.write_packet(&eof_packet()).await
}

/// Resolve a select's columns through the inference engine + user rules.
fn resolve_cols(shape: &ResultShape, cfg: &Config) -> Vec<Resolved> {
    shape
        .columns
        .iter()
        .map(|c| Resolved::from_spec(c, &cfg.rules))
        .collect()
}

/// Column-count packet, the column definitions, and the terminating EOF.
async fn send_result_header(conn: &mut Conn, cols: &[Resolved]) -> std::io::Result<()> {
    let mut count = BytesMut::new();
    put_lenenc_int(&mut count, cols.len() as u64);
    conn.write_packet(&count).await?;
    for c in cols {
        let def = column_def(c);
        conn.write_packet(&def).await?;
    }
    conn.write_packet(&eof_packet()).await
}

/// Stream up to `crush.max_rows` text rows in chunks, flushing each chunk, then
/// an EOF. O(1) server memory. Aborts (returns Err) if the client stops reading.
async fn crush_stream(
    conn: &mut Conn,
    cols: &[Resolved],
    cfg: &Config,
    sql: &str,
    peer: std::net::SocketAddr,
) -> std::io::Result<()> {
    send_result_header(conn, cols).await?;
    let mut rng = seed_rng(cfg, sql);
    let max = cfg.crush.max_rows;
    let mut sent: u64 = 0;
    while sent < max {
        let this = CRUSH_CHUNK.min(max - sent);
        for _ in 0..this {
            let row = text_row(cols, &mut rng, cfg.gen_ctx());
            if let Err(e) = conn.write_packet(&row).await {
                warn!(%peer, rows = sent, "crush aborted: client gave up");
                return Err(e);
            }
        }
        sent += this;
    }
    info!(%peer, rows = sent, "crush complete");
    conn.write_packet(&eof_packet()).await
}
