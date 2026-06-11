//! PostgreSQL wire protocol frontend (protocol 3.0, simple query flow).
//!
//! Supported: SSLRequest/GSSENCRequest (declined), StartupMessage, trust
//! auth, simple query ('Q'), Terminate ('X'). Extended-protocol messages get
//! a polite ErrorResponse so drivers fail loudly instead of hanging.

use std::collections::HashMap;
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

/// Rows flushed to the wire per write during a crush stream. Keeps server-side
/// memory flat no matter how many rows we promise the client.
const CRUSH_CHUNK: u64 = 1000;

/// A deliberately wide, type-diverse schema synthesised for `SELECT *` against
/// an unknown table — exercises many client codepaths at once.
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

const PROTO_V3: i32 = 196608;
const SSL_REQUEST: i32 = 80877103;
const GSSENC_REQUEST: i32 = 80877104;
const CANCEL_REQUEST: i32 = 80877102;
const MAX_MSG: usize = 16 * 1024 * 1024;

fn oid(wt: WireType) -> u32 {
    match wt {
        WireType::Bool => 16,
        WireType::Int8 => 20,
        WireType::Int4 => 23,
        WireType::Text => 25,
        WireType::Json => 114,
        WireType::Float8 => 701,
        WireType::Date => 1082,
        WireType::Time => 1083,
        WireType::Timestamp => 1114,
        WireType::Numeric => 1700,
        WireType::Uuid => 2950,
    }
}

/// Microseconds between the Unix epoch (1970) and the Postgres epoch (2000).
const PG_EPOCH_OFFSET_SECS: i64 = 946_684_800;
/// Days between the two epochs.
const PG_EPOCH_OFFSET_DAYS: i64 = 10957;

/// Re-encode a generated text value into the Postgres *binary* wire format for
/// its type. Generated text is always canonical, so parsing back is exact and
/// the fallbacks below never fire in normal operation — they only trigger on a
/// `--fuzz` ghost value, where an out-of-range extreme is *more* antagonistic
/// to the client's decoder than a tame zero would be.
fn text_to_binary(wt: WireType, text: &str) -> Vec<u8> {
    match wt {
        // 0/1 normally; an unrecognised (fuzzed) bool becomes an invalid 2.
        WireType::Bool => {
            let b = match text {
                "t" | "true" => 1,
                "f" | "false" => 0,
                _ => 2,
            };
            vec![b]
        }
        WireType::Int4 => text
            .parse::<i32>()
            .unwrap_or(i32::MIN)
            .to_be_bytes()
            .to_vec(),
        WireType::Int8 => text
            .parse::<i64>()
            .unwrap_or(i64::MIN)
            .to_be_bytes()
            .to_vec(),
        // Numeric is remapped to float8 for the binary path (see `binarize`).
        WireType::Float8 | WireType::Numeric => text
            .parse::<f64>()
            .unwrap_or(f64::NAN)
            .to_be_bytes()
            .to_vec(),
        WireType::Text | WireType::Json => text.as_bytes().to_vec(),
        WireType::Uuid => {
            let mut out = Vec::with_capacity(16);
            let hex: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
            for pair in hex.chunks(2).take(16) {
                let hi = (pair[0] as char).to_digit(16).unwrap_or(0);
                let lo = pair
                    .get(1)
                    .map_or(0, |c| (*c as char).to_digit(16).unwrap_or(0));
                out.push((hi * 16 + lo) as u8);
            }
            out.resize(16, 0);
            out
        }
        WireType::Date => {
            let days = parse_ymd(text)
                .map(|(y, m, d)| crate::generate::days_from_civil(y, m, d) - PG_EPOCH_OFFSET_DAYS);
            (days.unwrap_or(i32::MAX as i64) as i32)
                .to_be_bytes()
                .to_vec()
        }
        WireType::Time => {
            let micros = parse_hms(text).map(|(h, m, s)| (h * 3600 + m * 60 + s) * 1_000_000);
            micros.unwrap_or(i64::MAX).to_be_bytes().to_vec()
        }
        WireType::Timestamp => {
            let micros = parse_timestamp(text).unwrap_or(i64::MAX);
            micros.to_be_bytes().to_vec()
        }
    }
}

fn parse_ymd(s: &str) -> Option<(i64, u32, u32)> {
    let mut it = s.split('-');
    let y = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let d = it.next()?.parse().ok()?;
    Some((y, m, d))
}

fn parse_hms(s: &str) -> Option<(i64, i64, i64)> {
    let mut it = s.split(':');
    let h = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let sec = it.next()?.parse().ok()?;
    Some((h, m, sec))
}

/// "YYYY-MM-DD HH:MM:SS" -> microseconds since the Postgres epoch (2000-01-01).
fn parse_timestamp(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(' ')?;
    let (y, m, d) = parse_ymd(date)?;
    let (h, mi, se) = parse_hms(time)?;
    let days = crate::generate::days_from_civil(y, m, d);
    let secs = days * 86400 + h * 3600 + mi * 60 + se - PG_EPOCH_OFFSET_SECS;
    Some(secs * 1_000_000)
}

/// A cursor over a message payload with bounds-checked, saturating reads —
/// malformed input yields defaults rather than panicking.
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    fn u8(&mut self) -> u8 {
        let v = self.b.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        v
    }
    fn i16(&mut self) -> i16 {
        let mut a = [0u8; 2];
        for x in &mut a {
            *x = self.u8();
        }
        i16::from_be_bytes(a)
    }
    fn i32(&mut self) -> i32 {
        let mut a = [0u8; 4];
        for x in &mut a {
            *x = self.u8();
        }
        i32::from_be_bytes(a)
    }
    /// Read a NUL-terminated string.
    fn cstr(&mut self) -> String {
        let start = self.pos.min(self.b.len());
        let end = self.b[start..]
            .iter()
            .position(|&c| c == 0)
            .map_or(self.b.len(), |i| start + i);
        let s = String::from_utf8_lossy(&self.b[start..end]).into_owned();
        self.pos = (end + 1).min(self.b.len() + 1);
        s
    }
    /// Skip `n` bytes.
    fn skip(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n);
    }
}

fn put_msg(buf: &mut BytesMut, tag: u8, body: impl FnOnce(&mut BytesMut)) {
    buf.put_u8(tag);
    let len_pos = buf.len();
    buf.put_i32(0);
    body(buf);
    let len = (buf.len() - len_pos) as i32;
    buf[len_pos..len_pos + 4].copy_from_slice(&len.to_be_bytes());
}

fn put_cstr(buf: &mut BytesMut, s: &str) {
    buf.put_slice(s.as_bytes());
    buf.put_u8(0);
}

fn put_parameter_status(buf: &mut BytesMut, k: &str, v: &str) {
    put_msg(buf, b'S', |b| {
        put_cstr(b, k);
        put_cstr(b, v);
    });
}

fn put_ready(buf: &mut BytesMut) {
    put_msg(buf, b'Z', |b| b.put_u8(b'I'));
}

fn put_error(buf: &mut BytesMut, code: &str, msg: &str) {
    put_msg(buf, b'E', |b| {
        b.put_u8(b'S');
        put_cstr(b, "ERROR");
        b.put_u8(b'V');
        put_cstr(b, "ERROR");
        b.put_u8(b'C');
        put_cstr(b, code);
        b.put_u8(b'M');
        put_cstr(b, msg);
        b.put_u8(0);
    });
}

fn put_notice(buf: &mut BytesMut, msg: &str) {
    put_msg(buf, b'N', |b| {
        b.put_u8(b'S');
        put_cstr(b, "NOTICE");
        b.put_u8(b'V');
        put_cstr(b, "NOTICE");
        b.put_u8(b'C');
        put_cstr(b, "00000");
        b.put_u8(b'M');
        put_cstr(b, msg);
        b.put_u8(0);
    });
}

fn put_command_complete(buf: &mut BytesMut, tag: &str) {
    put_msg(buf, b'C', |b| put_cstr(b, tag));
}

pub async fn handle(mut stream: TcpStream, shared: Arc<Shared>) -> std::io::Result<()> {
    let peer = stream.peer_addr()?;

    // --- startup phase: decline SSL/GSS until we get a real StartupMessage
    loop {
        let len = stream.read_i32().await? as usize;
        if !(8..=MAX_MSG).contains(&len) {
            return Ok(());
        }
        let code = stream.read_i32().await?;
        match code {
            SSL_REQUEST | GSSENC_REQUEST => {
                stream.write_all(b"N").await?;
            }
            CANCEL_REQUEST => return Ok(()),
            PROTO_V3 => {
                let mut params = vec![0u8; len - 8];
                stream.read_exact(&mut params).await?;
                let mut user = "ghost".to_string();
                let mut database = String::new();
                let mut it = params.split(|b| *b == 0);
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    let k = String::from_utf8_lossy(k);
                    let v = String::from_utf8_lossy(v).into_owned();
                    match k.as_ref() {
                        "user" => user = v,
                        "database" => database = v,
                        _ => {}
                    }
                }
                if database.is_empty() {
                    database = user.clone();
                }
                info!(%peer, user, database, "session open (every database exists here)");
                break;
            }
            other => {
                debug!(%peer, other, "unknown startup code");
                return Ok(());
            }
        }
    }

    // --- auth (trust: everyone is welcome) + session parameters
    let mut buf = BytesMut::with_capacity(4096);
    put_msg(&mut buf, b'R', |b| b.put_i32(0)); // AuthenticationOk
    put_parameter_status(&mut buf, "server_version", "16.3 (EtherealDB 0.1.0)");
    put_parameter_status(&mut buf, "server_encoding", "UTF8");
    put_parameter_status(&mut buf, "client_encoding", "UTF8");
    put_parameter_status(&mut buf, "DateStyle", "ISO, MDY");
    put_parameter_status(&mut buf, "integer_datetimes", "on");
    put_parameter_status(&mut buf, "standard_conforming_strings", "on");
    put_parameter_status(&mut buf, "TimeZone", "UTC");
    put_msg(&mut buf, b'K', |b| {
        b.put_i32(rand::random::<i32>().abs());
        b.put_i32(rand::random());
    });
    put_notice(
        &mut buf,
        "welcome to EtherealDB — every query succeeds, no data is real",
    );
    put_ready(&mut buf);
    stream.write_all(&buf).await?;

    // --- command phase
    // Extended-protocol state: prepared statements (name -> SQL) and bound
    // portals. The unnamed statement/portal use the empty-string key.
    let mut statements: HashMap<String, String> = HashMap::new();
    let mut portals: HashMap<String, Portal> = HashMap::new();

    loop {
        let tag = match stream.read_u8().await {
            Ok(t) => t,
            Err(_) => return Ok(()), // client hung up
        };
        let len = stream.read_i32().await? as usize;
        if !(4..=MAX_MSG).contains(&len) {
            return Ok(());
        }
        let mut payload = vec![0u8; len - 4];
        stream.read_exact(&mut payload).await?;

        // --- ghosts: haunt query-bearing messages ('Q' simple, 'E' execute)
        let ghosts = &shared.cfg.ghosts;
        if ghosts.haunting() && matches!(tag, b'Q' | b'E') {
            ghosts.maybe_latency().await;
            if ghosts.maybe_drop() {
                debug!(%peer, "ghost: dropping connection");
                return Ok(());
            }
            if ghosts.maybe_error() {
                debug!(%peer, "ghost: injecting error");
                let mut e = BytesMut::new();
                put_error(&mut e, "58000", "a ghost ate your query");
                // After a simple query the server owns ReadyForQuery; in the
                // extended protocol the client's Sync drives it instead.
                if tag == b'Q' {
                    put_ready(&mut e);
                }
                stream.write_all(&e).await?;
                continue;
            }
        }

        let mut out = BytesMut::with_capacity(8192);
        match tag {
            // --- simple query
            b'Q' => {
                let sql = String::from_utf8_lossy(payload.strip_suffix(&[0]).unwrap_or(&payload))
                    .into_owned();
                debug!(%peer, %sql, "query");
                let stmts = shape::split_statements(&sql);
                if stmts.is_empty() {
                    put_msg(&mut out, b'I', |_| {}); // EmptyQueryResponse
                } else {
                    for stmt in stmts {
                        respond_statement(&mut stream, &mut out, stmt, &shared, peer).await?;
                    }
                }
                put_ready(&mut out);
            }

            // --- extended query: Parse
            b'P' => {
                let mut r = Reader::new(&payload);
                let name = r.cstr();
                let query = r.cstr();
                statements.insert(name, query);
                put_msg(&mut out, b'1', |_| {}); // ParseComplete
            }

            // --- extended query: Bind
            b'B' => {
                let mut r = Reader::new(&payload);
                let portal = r.cstr();
                let stmt_name = r.cstr();
                let n_param_formats = r.i16().max(0) as usize;
                for _ in 0..n_param_formats {
                    r.i16();
                }
                let n_params = r.i16().max(0) as usize;
                for _ in 0..n_params {
                    let plen = r.i32();
                    if plen > 0 {
                        r.skip(plen as usize); // we never read parameter values
                    }
                }
                let n_result_formats = r.i16().max(0) as usize;
                let result_formats: Vec<i16> = (0..n_result_formats).map(|_| r.i16()).collect();
                let sql = statements.get(&stmt_name).cloned().unwrap_or_default();
                portals.insert(
                    portal,
                    Portal {
                        sql,
                        result_formats,
                    },
                );
                put_msg(&mut out, b'2', |_| {}); // BindComplete
            }

            // --- extended query: Describe
            b'D' => {
                let mut r = Reader::new(&payload);
                let kind = r.u8();
                let name = r.cstr();
                let sql = if kind == b'S' {
                    statements.get(&name).cloned()
                } else {
                    portals.get(&name).map(|p| p.sql.clone())
                };
                let sql = sql.unwrap_or_default();
                let shape = shape::extract(&sql);
                if kind == b'S' {
                    // ParameterDescription
                    let params = shape::param_types(&sql, &shared.cfg.rules);
                    put_msg(&mut out, b't', |b| {
                        b.put_i16(params.len() as i16);
                        for wt in &params {
                            b.put_u32(oid(*wt));
                        }
                    });
                }
                if shape.kind == StmtKind::Select {
                    let cols = binarize(resolve_cols(&shape, &shared.cfg));
                    put_row_description(&mut out, &cols);
                } else {
                    put_msg(&mut out, b'n', |_| {}); // NoData
                }
            }

            // --- extended query: Execute
            b'E' => {
                let mut r = Reader::new(&payload);
                let portal = r.cstr();
                let max_rows = r.i32();
                let portal = portals.get(&portal).cloned().unwrap_or_default();
                execute_portal(&mut stream, &mut out, &portal, max_rows, &shared, peer).await?;
            }

            // --- extended query: Close
            b'C' => {
                let mut r = Reader::new(&payload);
                let kind = r.u8();
                let name = r.cstr();
                if kind == b'S' {
                    statements.remove(&name);
                } else {
                    portals.remove(&name);
                }
                put_msg(&mut out, b'3', |_| {}); // CloseComplete
            }

            b'X' => return Ok(()),       // Terminate
            b'S' => put_ready(&mut out), // Sync
            b'H' => {}                   // Flush — out is flushed below regardless
            _ => {
                debug!(%peer, tag = (tag as char).to_string(), "ignoring message");
            }
        }
        if !out.is_empty() {
            stream.write_all(&out).await?;
        }
    }
}

/// A bound portal: the SQL it came from plus the result format codes the client
/// requested in Bind (empty = all text, len 1 = uniform, else per-column).
#[derive(Clone, Default)]
struct Portal {
    sql: String,
    result_formats: Vec<i16>,
}

/// Build the RowDescription ('T') message for a set of resolved columns.
fn put_row_description(out: &mut BytesMut, cols: &[Resolved]) {
    put_msg(out, b'T', |b| {
        b.put_i16(cols.len() as i16);
        for c in cols {
            put_cstr(b, &c.name);
            b.put_i32(0); // table oid
            b.put_i16(0); // attnum
            b.put_u32(oid(c.wt));
            b.put_i16(-1); // typlen
            b.put_i32(-1); // typmod
            b.put_i16(0); // text format
        }
    });
}

/// Append one DataRow ('D') of freshly generated values, in text format.
fn put_data_row(out: &mut BytesMut, cols: &[Resolved], rng: &mut impl Rng, g: Gen) {
    put_data_row_fmt(out, cols, rng, &[], g);
}

/// The result format for column `i` given Bind's format codes: empty = all
/// text, length 1 = that code for every column, otherwise per-column.
fn format_for(formats: &[i16], i: usize) -> i16 {
    match formats.len() {
        0 => 0,
        1 => formats[0],
        _ => formats.get(i).copied().unwrap_or(0),
    }
}

/// Append one DataRow honoring per-column result format codes (0 text, 1 binary).
fn put_data_row_fmt(
    out: &mut BytesMut,
    cols: &[Resolved],
    rng: &mut impl Rng,
    formats: &[i16],
    g: Gen,
) {
    put_msg(out, b'D', |b| {
        b.put_i16(cols.len() as i16);
        for (i, c) in cols.iter().enumerate() {
            let text = match &c.literal {
                Some(v) => v.clone(),
                None => gen_value(c.st, c.wt, rng, g),
            };
            if format_for(formats, i) == 1 {
                let bin = text_to_binary(c.wt, &text);
                b.put_i32(bin.len() as i32);
                b.put_slice(&bin);
            } else {
                b.put_i32(text.len() as i32);
                b.put_slice(text.as_bytes());
            }
        }
    });
}

/// Remap columns for the extended/binary path: Postgres binary `numeric` uses a
/// fiddly base-10000 encoding, so we advertise and encode money/percent columns
/// as float8 instead — close enough for a database that isn't there.
fn binarize(mut cols: Vec<Resolved>) -> Vec<Resolved> {
    for c in &mut cols {
        if c.wt == WireType::Numeric {
            c.wt = WireType::Float8;
        }
    }
    cols
}

fn seed_rng(cfg: &Config, stmt: &str) -> ChaCha8Rng {
    match cfg.seed {
        Some(seed) => ChaCha8Rng::seed_from_u64(seed ^ fnv64(stmt.as_bytes())),
        None => ChaCha8Rng::from_os_rng(),
    }
}

/// Respond to one statement. Most responses are buffered into `out`; a crushed
/// query streams directly to `stream` (after flushing `out` to preserve order).
async fn respond_statement(
    stream: &mut TcpStream,
    out: &mut BytesMut,
    stmt: &str,
    shared: &Shared,
    peer: std::net::SocketAddr,
) -> std::io::Result<()> {
    let cfg = &shared.cfg;
    let shape = shape::extract(stmt);

    let class = if cfg.crush.enabled {
        shape.crush_class(cfg.crush.threshold)
    } else {
        CrushClass::Safe
    };

    match class {
        CrushClass::Crush { .. } if !cfg.crush.warn_only => {
            // Acquire a crush slot; if we're at capacity, fall back to normal.
            match shared.crush_slots.try_acquire() {
                Ok(_permit) => {
                    warn!(%peer, reasons = class.reasons(), query = truncate(stmt), "CRUSH");
                    if !out.is_empty() {
                        stream.write_all(out).await?;
                        out.clear();
                    }
                    let sent = crush_stream(stream, &shape, cfg, stmt).await;
                    match sent {
                        Ok(n) => info!(%peer, rows = n, "crush complete"),
                        Err((n, e)) => {
                            warn!(%peer, rows = n, "crush aborted: client gave up");
                            return Err(e);
                        }
                    }
                    return Ok(());
                }
                Err(_) => {
                    put_notice(
                        out,
                        "crush mode at capacity — enjoy a normal answer instead",
                    );
                }
            }
        }
        CrushClass::Crush { .. } => {
            // warn-only: name and shame, then answer normally.
            warn!(%peer, reasons = class.reasons(), query = truncate(stmt), "unsafe query (warn-only)");
            put_notice(
                out,
                &format!(
                    "unsafe query ({}); crush mode is warn-only",
                    class.reasons()
                ),
            );
        }
        CrushClass::Warn { .. } => {
            put_notice(
                out,
                &format!(
                    "loose query ({}) — consider a column list, WHERE, or LIMIT",
                    class.reasons()
                ),
            );
        }
        CrushClass::Safe => {}
    }

    normal_response(out, &shape, cfg, stmt);
    Ok(())
}

fn normal_response(out: &mut BytesMut, shape: &ResultShape, cfg: &Config, stmt: &str) {
    let mut rng = seed_rng(cfg, stmt);
    match &shape.kind {
        StmtKind::Empty => put_msg(out, b'I', |_| {}),
        StmtKind::Select => {
            // No FROM clause (SELECT 1, SELECT now()) yields exactly one row,
            // like the real thing; system-catalog queries yield zero.
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

            let cols = resolve_cols(shape, cfg);
            put_row_description(out, &cols);
            for _ in 0..n {
                put_data_row(out, &cols, &mut rng, cfg.gen_ctx());
            }
            put_command_complete(out, &format!("SELECT {n}"));
        }
        StmtKind::Insert => put_command_complete(out, "INSERT 0 1"),
        StmtKind::Update => {
            let n: u32 = rng.random_range(0..50);
            put_command_complete(out, &format!("UPDATE {n}"));
        }
        StmtKind::Delete => {
            let n: u32 = rng.random_range(0..50);
            put_command_complete(out, &format!("DELETE {n}"));
        }
        StmtKind::Command(tag) => put_command_complete(out, tag),
    }
}

/// Extended-protocol Execute: produce DataRows + CommandComplete for a bound
/// portal (RowDescription was already sent in response to Describe). Honors the
/// portal's result format codes and applies crush mode, just like simple query.
async fn execute_portal(
    stream: &mut TcpStream,
    out: &mut BytesMut,
    portal: &Portal,
    max_rows: i32,
    shared: &Shared,
    peer: std::net::SocketAddr,
) -> std::io::Result<()> {
    let cfg = &shared.cfg;
    let sql = &portal.sql;
    let formats = &portal.result_formats;
    let shape = shape::extract(sql);

    let class = if cfg.crush.enabled {
        shape.crush_class(cfg.crush.threshold)
    } else {
        CrushClass::Safe
    };

    if let StmtKind::Select = shape.kind {
        let cols = binarize(resolve_cols(&shape, cfg));

        // Crush: stream the avalanche directly (no extra RowDescription).
        if matches!(class, CrushClass::Crush { .. }) && !cfg.crush.warn_only {
            if let Ok(_permit) = shared.crush_slots.try_acquire() {
                warn!(%peer, reasons = class.reasons(), query = truncate(sql), "CRUSH (extended)");
                if !out.is_empty() {
                    stream.write_all(out).await?;
                    out.clear();
                }
                let mut notice = BytesMut::with_capacity(128);
                put_notice(&mut notice, CRUSH_NOTICE);
                stream.write_all(&notice).await?;
                let rng = seed_rng(cfg, sql);
                return match stream_rows(
                    stream,
                    &cols,
                    formats,
                    cfg.crush.max_rows,
                    rng,
                    cfg.gen_ctx(),
                )
                .await
                {
                    Ok(n) => {
                        info!(%peer, rows = n, "crush complete");
                        Ok(())
                    }
                    Err((n, e)) => {
                        warn!(%peer, rows = n, "crush aborted: client gave up");
                        Err(e)
                    }
                };
            }
            put_notice(
                out,
                "crush mode at capacity — enjoy a normal answer instead",
            );
        } else if let CrushClass::Crush { .. } = class {
            warn!(%peer, reasons = class.reasons(), query = truncate(sql), "unsafe query (warn-only)");
            put_notice(
                out,
                &format!(
                    "unsafe query ({}); crush mode is warn-only",
                    class.reasons()
                ),
            );
        } else if let CrushClass::Warn { .. } = class {
            put_notice(
                out,
                &format!(
                    "loose query ({}) — consider a column list, WHERE, or LIMIT",
                    class.reasons()
                ),
            );
        }

        // Normal response.
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
        if max_rows > 0 {
            n = n.min(max_rows as usize);
        }
        for _ in 0..n {
            put_data_row_fmt(out, &cols, &mut rng, formats, cfg.gen_ctx());
        }
        put_command_complete(out, &format!("SELECT {n}"));
        return Ok(());
    }

    // Non-SELECT: same tags as the simple-query path.
    let mut rng = seed_rng(cfg, sql);
    match &shape.kind {
        StmtKind::Empty => put_msg(out, b'I', |_| {}),
        StmtKind::Insert => put_command_complete(out, "INSERT 0 1"),
        StmtKind::Update => {
            let n: u32 = rng.random_range(0..50);
            put_command_complete(out, &format!("UPDATE {n}"));
        }
        StmtKind::Delete => {
            let n: u32 = rng.random_range(0..50);
            put_command_complete(out, &format!("DELETE {n}"));
        }
        StmtKind::Command(tag) => put_command_complete(out, tag),
        StmtKind::Select => unreachable!("handled above"),
    }
    Ok(())
}

/// Simple-query crush: send a NOTICE + RowDescription, then the avalanche.
///
/// On success returns the row count sent. On write failure returns the count
/// sent so far plus the error (the client has almost certainly given up).
async fn crush_stream(
    stream: &mut TcpStream,
    shape: &ResultShape,
    cfg: &Config,
    stmt: &str,
) -> Result<u64, (u64, std::io::Error)> {
    // For `SELECT *` against an unknown table, synthesise a wide, diverse
    // schema; otherwise honour the columns the client actually asked for.
    let cols: Vec<Resolved> = if shape.select_star {
        WIDE_CRUSH_COLUMNS
            .iter()
            .map(|n| Resolved::from_name(n, &cfg.rules))
            .collect()
    } else {
        resolve_cols(shape, cfg)
    };

    let mut header = BytesMut::with_capacity(256);
    put_notice(&mut header, CRUSH_NOTICE);
    put_row_description(&mut header, &cols);
    if let Err(e) = stream.write_all(&header).await {
        return Err((0, e));
    }
    let rng = seed_rng(cfg, stmt);
    stream_rows(stream, &cols, &[], cfg.crush.max_rows, rng, cfg.gen_ctx()).await
}

const CRUSH_NOTICE: &str = "CRUSH MODE: this query asked for everything — here it comes";

/// Resolve a select's columns through the inference engine + user rules.
fn resolve_cols(shape: &ResultShape, cfg: &Config) -> Vec<Resolved> {
    shape
        .columns
        .iter()
        .map(|c| Resolved::from_spec(c, &cfg.rules))
        .collect()
}

/// Stream up to `max` DataRows in 1000-row chunks, then a CommandComplete.
/// O(1) memory: one chunk buffer, reused. Format codes select text/binary.
/// RowDescription (and any NOTICE) must already have been sent by the caller.
async fn stream_rows(
    stream: &mut TcpStream,
    cols: &[Resolved],
    formats: &[i16],
    max: u64,
    mut rng: ChaCha8Rng,
    g: Gen<'_>,
) -> Result<u64, (u64, std::io::Error)> {
    let mut sent: u64 = 0;
    let mut chunk = BytesMut::with_capacity(64 * 1024);
    while sent < max {
        let this = CRUSH_CHUNK.min(max - sent);
        chunk.clear();
        for _ in 0..this {
            put_data_row_fmt(&mut chunk, cols, &mut rng, formats, g);
        }
        if let Err(e) = stream.write_all(&chunk).await {
            return Err((sent, e));
        }
        sent += this;
    }
    let mut tail = BytesMut::with_capacity(32);
    put_command_complete(&mut tail, &format!("SELECT {sent}"));
    if let Err(e) = stream.write_all(&tail).await {
        return Err((sent, e));
    }
    Ok(sent)
}

fn truncate(s: &str) -> String {
    const MAX: usize = 500;
    if s.len() <= MAX {
        return s.to_string();
    }
    let cut = (0..=MAX)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    format!("{}…", &s[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_roundtrips_canonical_values() {
        // Normal generated text encodes exactly (the fuzz fallbacks don't fire).
        assert_eq!(text_to_binary(WireType::Int4, "42"), 42i32.to_be_bytes());
        assert_eq!(text_to_binary(WireType::Int8, "-7"), (-7i64).to_be_bytes());
        assert_eq!(text_to_binary(WireType::Bool, "t"), vec![1]);
        assert_eq!(text_to_binary(WireType::Bool, "f"), vec![0]);
        // 2000-01-01 is day 0 of the Postgres epoch.
        assert_eq!(
            text_to_binary(WireType::Date, "2000-01-01"),
            0i32.to_be_bytes()
        );
        assert_eq!(
            text_to_binary(WireType::Time, "01:00:00"),
            3_600_000_000i64.to_be_bytes()
        );
    }

    #[test]
    fn binary_fuzz_values_become_antagonistic_extremes() {
        // A fuzzed value that can't parse encodes as an out-of-range extreme,
        // not a tame zero — so --fuzz bites binary clients too.
        assert_eq!(
            text_to_binary(WireType::Int8, "NaN"),
            i64::MIN.to_be_bytes()
        );
        assert_eq!(
            text_to_binary(WireType::Int4, "0x7fffffff"),
            i32::MIN.to_be_bytes()
        );
        // An unparseable float falls back to NaN. (Note "Infinity"/"NaN" *do*
        // parse to real inf/nan in Rust — themselves antagonistic.)
        let f = f64::from_be_bytes(
            text_to_binary(WireType::Float8, "  1 2 ")
                .try_into()
                .unwrap(),
        );
        assert!(f.is_nan());
        let inf = f64::from_be_bytes(
            text_to_binary(WireType::Float8, "Infinity")
                .try_into()
                .unwrap(),
        );
        assert!(inf.is_infinite());
        assert_eq!(text_to_binary(WireType::Bool, "maybe"), vec![2]);
        assert_eq!(
            text_to_binary(WireType::Timestamp, "not-a-date"),
            i64::MAX.to_be_bytes()
        );
    }
}
