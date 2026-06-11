//! End-to-end for the Redis/RESP frontend, driven by a minimal RESP client
//! written here (no external redis dependency).

use etherealdb::config::{Config, CrushConfig};
use etherealdb::server::{self, Proto, Shared};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

async fn start(cfg: Config) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(server::run(listener, Shared::new(cfg), Proto::Redis));
    port
}

struct Client {
    io: BufReader<TcpStream>,
}

impl Client {
    async fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        Client {
            io: BufReader::new(stream),
        }
    }

    /// Send a command as a RESP array of bulk strings.
    async fn send(&mut self, parts: &[&str]) {
        let mut buf = format!("*{}\r\n", parts.len());
        for p in parts {
            buf.push_str(&format!("${}\r\n{}\r\n", p.len(), p));
        }
        self.io.write_all(buf.as_bytes()).await.unwrap();
    }

    async fn line(&mut self) -> String {
        let mut s = String::new();
        self.io.read_line(&mut s).await.unwrap();
        s.trim_end().to_string()
    }

    /// Read one reply, returning a flat description for assertions.
    async fn reply(&mut self) -> Reply {
        let line = self.line().await;
        let (tag, rest) = line.split_at(1);
        match tag {
            "+" => Reply::Simple(rest.to_string()),
            "-" => Reply::Error(rest.to_string()),
            ":" => Reply::Int(rest.parse().unwrap()),
            "$" => {
                let len: i64 = rest.parse().unwrap();
                if len < 0 {
                    return Reply::Nil;
                }
                let mut data = vec![0u8; len as usize + 2];
                self.io.read_exact(&mut data).await.unwrap();
                data.truncate(len as usize);
                Reply::Bulk(String::from_utf8_lossy(&data).into_owned())
            }
            "*" => {
                let n: i64 = rest.parse().unwrap();
                let mut items = Vec::new();
                for _ in 0..n.max(0) {
                    items.push(Box::pin(self.reply()).await);
                }
                Reply::Array(items)
            }
            other => panic!("unexpected reply tag {other}"),
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // some variants/fields exist only to be matched in assertions
enum Reply {
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(String),
    Nil,
    Array(Vec<Reply>),
}

impl Reply {
    fn bulk(&self) -> &str {
        match self {
            Reply::Bulk(s) => s,
            other => panic!("expected bulk string, got {other:?}"),
        }
    }
    fn array(&self) -> &[Reply] {
        match self {
            Reply::Array(v) => v,
            other => panic!("expected array, got {other:?}"),
        }
    }
}

fn seeded() -> Config {
    Config {
        seed: Some(7),
        ..Config::default()
    }
}

#[tokio::test]
async fn ping_and_echo() {
    let port = start(Config::default()).await;
    let mut c = Client::connect(port).await;
    c.send(&["PING"]).await;
    assert!(matches!(c.reply().await, Reply::Simple(s) if s == "PONG"));
    c.send(&["ECHO", "boo"]).await;
    assert_eq!(c.reply().await.bulk(), "boo");
}

#[tokio::test]
async fn get_infers_value_from_key() {
    let port = start(seeded()).await;
    let mut c = Client::connect(port).await;

    c.send(&["GET", "user:42:email"]).await;
    assert!(
        c.reply().await.bulk().contains('@'),
        "email key should yield an email"
    );

    c.send(&["GET", "cart:9:total"]).await;
    let total = c.reply().await.bulk().to_string();
    assert!(
        total.parse::<f64>().is_ok(),
        "total should be numeric: {total}"
    );

    c.send(&["GET", "session:1:created_at"]).await;
    assert_eq!(
        c.reply().await.bulk().len(),
        19,
        "created_at should be a timestamp"
    );
}

#[tokio::test]
async fn set_is_ok_and_mget_is_array() {
    let port = start(seeded()).await;
    let mut c = Client::connect(port).await;

    c.send(&["SET", "k", "v"]).await;
    assert!(matches!(c.reply().await, Reply::Simple(s) if s == "OK"));

    c.send(&["MGET", "a:email", "b:status", "c:id"]).await;
    let r = c.reply().await;
    let items = r.array();
    assert_eq!(items.len(), 3);
    assert!(items[0].bulk().contains('@'));
    assert!(items[2].bulk().parse::<i64>().is_ok());
}

#[tokio::test]
async fn keys_returns_array_of_fake_keys() {
    let port = start(seeded()).await;
    let mut c = Client::connect(port).await;
    c.send(&["KEYS", "*"]).await;
    let r = c.reply().await;
    let items = r.array();
    assert!(!items.is_empty());
    assert!(
        items[0].bulk().contains(':'),
        "fake keys look like ns:id:field"
    );
}

#[tokio::test]
async fn keys_star_is_crushed() {
    let cfg = Config {
        crush: CrushConfig {
            enabled: true,
            max_rows: 12_000,
            ..CrushConfig::default()
        },
        ..Config::default()
    };
    let port = start(cfg).await;
    let mut c = Client::connect(port).await;
    c.send(&["KEYS", "*"]).await;
    let r = c.reply().await;
    assert_eq!(
        r.array().len(),
        12_000,
        "KEYS * should be crushed to max_rows"
    );
}

#[tokio::test]
async fn unknown_command_errors() {
    let port = start(Config::default()).await;
    let mut c = Client::connect(port).await;
    c.send(&["FLERP", "x"]).await;
    assert!(matches!(c.reply().await, Reply::Error(_)));
}

#[tokio::test]
async fn hget_infers_from_field_name() {
    let port = start(seeded()).await;
    let mut c = Client::connect(port).await;
    // HGET <key> <field> -> value inferred from the FIELD, not the key.
    c.send(&["HGET", "user:7", "email"]).await;
    assert!(
        c.reply().await.bulk().contains('@'),
        "HGET email field -> email"
    );

    c.send(&["HGET", "user:7", "created_at"]).await;
    assert_eq!(c.reply().await.bulk().len(), 19, "created_at -> timestamp");
}

#[tokio::test]
async fn bool_key_renders_as_one_or_zero() {
    let port = start(seeded()).await;
    let mut c = Client::connect(port).await;
    c.send(&["GET", "user:1:is_active"]).await;
    let v = c.reply().await.bulk().to_string();
    assert!(v == "1" || v == "0", "redis bool should be 1/0, got {v}");
}
