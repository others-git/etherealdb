//! Redis RESP frontend (RESP2). Redis is key-value, not tabular, so the
//! inference twist is different: the value's semantic type is guessed from the
//! **key name** — `GET user:42:email` returns an email, `GET cart:9:total`
//! returns money. The dangerous broad query here is `KEYS *`, which is the
//! crush trigger (the Redis analogue of `SELECT *`).
//!
//! Everything downstream — inference, value generation, themes, rules, crush —
//! is shared with the SQL frontends.

use std::sync::Arc;

use rand::seq::IndexedRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::config::{Config, fnv64};
use crate::generate::generate;
use crate::infer::WireType;
use crate::server::Shared;
use crate::shape::Resolved;
use crate::theme::ThemeData;

const CRUSH_CHUNK: u64 = 1000;

/// Field names tacked onto synthesised keys (`order:8421:status`).
static KEY_FIELDS: &[&str] = &[
    "id",
    "email",
    "status",
    "name",
    "created_at",
    "total",
    "count",
    "updated_at",
];

// --- RESP writers (into a reusable buffer) ---

fn simple(buf: &mut Vec<u8>, s: &str) {
    buf.push(b'+');
    buf.extend_from_slice(s.as_bytes());
    buf.extend_from_slice(b"\r\n");
}

fn error(buf: &mut Vec<u8>, s: &str) {
    buf.push(b'-');
    buf.extend_from_slice(s.as_bytes());
    buf.extend_from_slice(b"\r\n");
}

fn integer(buf: &mut Vec<u8>, n: i64) {
    buf.push(b':');
    buf.extend_from_slice(n.to_string().as_bytes());
    buf.extend_from_slice(b"\r\n");
}

fn bulk(buf: &mut Vec<u8>, data: &[u8]) {
    buf.push(b'$');
    buf.extend_from_slice(data.len().to_string().as_bytes());
    buf.extend_from_slice(b"\r\n");
    buf.extend_from_slice(data);
    buf.extend_from_slice(b"\r\n");
}

fn array_header(buf: &mut Vec<u8>, n: usize) {
    buf.push(b'*');
    buf.extend_from_slice(n.to_string().as_bytes());
    buf.extend_from_slice(b"\r\n");
}

// --- request parsing ---

type Args = Vec<Vec<u8>>;

/// Read one command: a RESP array of bulk strings, or a plain inline line.
/// `Ok(None)` means the client closed the connection.
async fn read_command<R>(r: &mut R) -> std::io::Result<Option<Args>>
where
    R: AsyncBufReadExt + AsyncReadExt + Unpin,
{
    loop {
        let Some(line) = read_line(r).await? else {
            return Ok(None);
        };
        if line.is_empty() {
            continue; // tolerate stray blank lines
        }
        if line[0] == b'*' {
            let count: i64 = parse_int(&line[1..]).unwrap_or(0);
            if count <= 0 {
                return Ok(Some(Vec::new()));
            }
            let mut args = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let Some(hdr) = read_line(r).await? else {
                    return Ok(None);
                };
                if hdr.first() != Some(&b'$') {
                    return Ok(Some(args)); // malformed; answer what we have
                }
                let len: i64 = parse_int(&hdr[1..]).unwrap_or(-1);
                if len < 0 {
                    args.push(Vec::new());
                    continue;
                }
                let mut data = vec![0u8; len as usize + 2]; // payload + CRLF
                r.read_exact(&mut data).await?;
                data.truncate(len as usize);
                args.push(data);
            }
            return Ok(Some(args));
        }
        // inline command: split on whitespace
        let args = line
            .split(|b| b.is_ascii_whitespace())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_vec())
            .collect();
        return Ok(Some(args));
    }
}

async fn read_line<R>(r: &mut R) -> std::io::Result<Option<Vec<u8>>>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut buf = Vec::new();
    if r.read_until(b'\n', &mut buf).await? == 0 {
        return Ok(None);
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    Ok(Some(buf))
}

fn parse_int(b: &[u8]) -> Option<i64> {
    std::str::from_utf8(b).ok()?.trim().parse().ok()
}

// --- value generation ---

fn seed_rng(cfg: &Config, key: &[u8]) -> ChaCha8Rng {
    match cfg.seed {
        Some(seed) => ChaCha8Rng::seed_from_u64(seed ^ fnv64(key)),
        None => ChaCha8Rng::from_os_rng(),
    }
}

/// Infer a value from a key by running its last `:`-delimited segment through
/// the inference engine (`user:42:email` -> the `email` generator).
fn value_for_key(key: &[u8], cfg: &Config, rng: &mut impl Rng) -> String {
    let key = String::from_utf8_lossy(key);
    let field = key.rsplit(':').next().unwrap_or(&key);
    let r = Resolved::from_name(field, &cfg.rules);
    let v = generate(r.st, rng, cfg.theme);
    // Redis stores strings; booleans read more naturally as 1/0.
    if r.wt == WireType::Bool {
        if v == "t" { "1".into() } else { "0".into() }
    } else {
        v
    }
}

fn fake_key(rng: &mut impl Rng, theme: &ThemeData) -> String {
    let ns = theme.nouns.choose(rng).copied().unwrap_or("key");
    let field = KEY_FIELDS.choose(rng).unwrap();
    format!("{ns}:{}:{field}", rng.random_range(1..1_000_000))
}

fn arg_str(a: &[u8]) -> String {
    String::from_utf8_lossy(a).into_owned()
}

pub async fn handle(stream: TcpStream, shared: Arc<Shared>) -> std::io::Result<()> {
    let peer = stream.peer_addr()?;
    let cfg = &shared.cfg;
    let mut io = BufReader::new(stream);
    info!(%peer, "redis session open (every key exists here)");

    loop {
        let Some(args) = read_command(&mut io).await? else {
            return Ok(()); // client hung up
        };
        if args.is_empty() {
            continue;
        }
        let cmd = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        debug!(%peer, %cmd, "redis command");

        // KEYS * under crush mode streams an avalanche of fake keys.
        if cmd == "KEYS" && cfg.crush.enabled && !cfg.crush.warn_only {
            let pattern = args.get(1).map(|p| arg_str(p)).unwrap_or_default();
            if pattern == "*"
                && let Ok(_permit) = shared.crush_slots.try_acquire()
            {
                warn!(%peer, "CRUSH (redis): KEYS *");
                crush_keys(&mut io, cfg, peer).await?;
                continue;
            }
        }

        let mut out = Vec::with_capacity(64);
        dispatch(&cmd, &args, cfg, &mut out);
        if out.is_empty() {
            continue; // QUIT handled by returning below
        }
        io.write_all(&out).await?;
        if cmd == "QUIT" {
            return Ok(());
        }
    }
}

fn dispatch(cmd: &str, args: &Args, cfg: &Config, out: &mut Vec<u8>) {
    let key = args.get(1).map(Vec::as_slice).unwrap_or(b"");
    match cmd {
        "PING" => match args.get(1) {
            Some(msg) => bulk(out, msg),
            None => simple(out, "PONG"),
        },
        "ECHO" => bulk(out, args.get(1).map(Vec::as_slice).unwrap_or(b"")),
        "QUIT" => simple(out, "OK"),
        "SELECT" | "AUTH" | "RESET" => simple(out, "OK"),
        "CLIENT" => simple(out, "OK"),

        // RESP3 negotiation: decline so the client stays on RESP2.
        "HELLO" => error(out, "NOPROTO unsupported protocol version"),

        // redis-cli probes these on connect; keep it happy with empties.
        "COMMAND" => array_header(out, 0),

        "SET" | "SETEX" | "PSETEX" | "MSET" | "GETSET" => simple(out, "OK"),
        "GET" | "GETDEL" => {
            let mut rng = seed_rng(cfg, key);
            bulk(out, value_for_key(key, cfg, &mut rng).as_bytes());
        }
        "MGET" => {
            array_header(out, args.len().saturating_sub(1));
            for k in &args[1..] {
                let mut rng = seed_rng(cfg, k);
                bulk(out, value_for_key(k, cfg, &mut rng).as_bytes());
            }
        }
        "STRLEN" => {
            let mut rng = seed_rng(cfg, key);
            integer(out, value_for_key(key, cfg, &mut rng).len() as i64);
        }
        "HGET" => {
            // field is the type signal for hashes: HGET user:1 email -> email
            let field = args.get(2).map(Vec::as_slice).unwrap_or(key);
            let mut rng = seed_rng(cfg, field);
            bulk(out, value_for_key(field, cfg, &mut rng).as_bytes());
        }
        "HGETALL" => {
            let mut rng = seed_rng(cfg, key);
            let n = rng.random_range(2..=5);
            array_header(out, n * 2);
            for _ in 0..n {
                let field = *KEY_FIELDS.choose(&mut rng).unwrap();
                bulk(out, field.as_bytes());
                bulk(
                    out,
                    value_for_key(field.as_bytes(), cfg, &mut rng).as_bytes(),
                );
            }
        }
        "HSET" | "HMSET" => simple(out, "OK"),
        "HKEYS" => {
            let mut rng = seed_rng(cfg, key);
            let n = rng.random_range(2..=5);
            array_header(out, n);
            for _ in 0..n {
                bulk(out, KEY_FIELDS.choose(&mut rng).unwrap().as_bytes());
            }
        }
        "KEYS" => {
            let mut rng = seed_rng(cfg, key);
            let n = rng.random_range(cfg.rows_min..=cfg.rows_max.max(cfg.rows_min));
            array_header(out, n);
            for _ in 0..n {
                bulk(out, fake_key(&mut rng, cfg.theme).as_bytes());
            }
        }
        "SCAN" => {
            // SCAN cursor ... -> [next-cursor, [keys]]; report 0 (done) + a page.
            let mut rng = seed_rng(cfg, key);
            let n = rng.random_range(cfg.rows_min..=cfg.rows_max.max(cfg.rows_min));
            array_header(out, 2);
            bulk(out, b"0");
            array_header(out, n);
            for _ in 0..n {
                bulk(out, fake_key(&mut rng, cfg.theme).as_bytes());
            }
        }
        // every key exists here, so EXISTS counts all the keys asked about
        "EXISTS" => integer(out, args.len().saturating_sub(1) as i64),
        "DEL" | "UNLINK" => integer(out, args.len().saturating_sub(1) as i64),
        "INCR" | "INCRBY" | "DECR" | "DECRBY" => {
            let mut rng = seed_rng(cfg, key);
            integer(out, rng.random_range(1..100_000));
        }
        "TTL" | "PTTL" => {
            let mut rng = seed_rng(cfg, key);
            integer(out, rng.random_range(1..86_400));
        }
        "EXPIRE" | "PEXPIRE" | "PERSIST" => integer(out, 1),
        "TYPE" => simple(out, "string"),
        "DBSIZE" => {
            let mut rng = seed_rng(cfg, b"dbsize");
            integer(out, rng.random_range(1_000..10_000_000));
        }
        "LLEN" | "SCARD" | "HLEN" | "ZCARD" => {
            let mut rng = seed_rng(cfg, key);
            integer(out, rng.random_range(0..1000));
        }
        "LPUSH" | "RPUSH" | "SADD" | "ZADD" => integer(out, 1),
        "LRANGE" | "SMEMBERS" => {
            let mut rng = seed_rng(cfg, key);
            let n = rng.random_range(0..=cfg.rows_max);
            array_header(out, n);
            for _ in 0..n {
                bulk(out, value_for_key(key, cfg, &mut rng).as_bytes());
            }
        }
        "CONFIG" => {
            // CONFIG GET <param> -> [param, value]
            let param = args.get(2).map(|p| arg_str(p)).unwrap_or_default();
            array_header(out, 2);
            bulk(out, param.as_bytes());
            bulk(out, b"");
        }
        "INFO" => {
            let body = "# Server\r\nredis_version:7.4.0-EtherealDB\r\nredis_mode:standalone\r\n";
            bulk(out, body.as_bytes());
        }
        _ => error(
            out,
            &format!("ERR unknown command '{}'", cmd.to_ascii_lowercase()),
        ),
    }
}

/// Stream a huge RESP array of fake keys (the array length is declared up front,
/// then the bulk strings are flushed in chunks — O(1) server memory).
async fn crush_keys(
    io: &mut BufReader<TcpStream>,
    cfg: &Config,
    peer: std::net::SocketAddr,
) -> std::io::Result<()> {
    let max = cfg.crush.max_rows;
    let mut header = Vec::new();
    array_header(&mut header, max as usize);
    io.write_all(&header).await?;

    let mut rng = seed_rng(cfg, b"KEYS *");
    let mut sent: u64 = 0;
    let mut chunk = Vec::with_capacity(64 * 1024);
    while sent < max {
        let this = CRUSH_CHUNK.min(max - sent);
        chunk.clear();
        for _ in 0..this {
            bulk(&mut chunk, fake_key(&mut rng, cfg.theme).as_bytes());
        }
        if let Err(e) = io.write_all(&chunk).await {
            warn!(%peer, keys = sent, "crush aborted: client gave up");
            return Err(e);
        }
        sent += this;
    }
    info!(%peer, keys = sent, "crush complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_resp_array() {
        let input = b"*2\r\n$3\r\nGET\r\n$11\r\nuser:1:mail\r\n";
        let mut r = BufReader::new(&input[..]);
        let args = read_command(&mut r).await.unwrap().unwrap();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], b"GET");
        assert_eq!(args[1], b"user:1:mail");
    }

    #[tokio::test]
    async fn parses_inline_command() {
        let input = b"PING hello\r\n";
        let mut r = BufReader::new(&input[..]);
        let args = read_command(&mut r).await.unwrap().unwrap();
        assert_eq!(args, vec![b"PING".to_vec(), b"hello".to_vec()]);
    }

    #[test]
    fn value_inferred_from_key_field() {
        let cfg = Config {
            seed: Some(1),
            ..Config::default()
        };
        let mut rng = seed_rng(&cfg, b"user:42:email");
        let v = value_for_key(b"user:42:email", &cfg, &mut rng);
        assert!(v.contains('@'), "expected email, got {v}");
    }

    #[test]
    fn get_emits_bulk_string() {
        let cfg = Config {
            seed: Some(2),
            ..Config::default()
        };
        let args = vec![b"GET".to_vec(), b"cart:9:total".to_vec()];
        let mut out = Vec::new();
        dispatch("GET", &args, &cfg, &mut out);
        assert_eq!(out[0], b'$', "GET should reply with a bulk string");
    }
}
