//! PostgreSQL wire protocol frontend (protocol 3.0, simple query flow).
//!
//! Supported: SSLRequest/GSSENCRequest (declined), StartupMessage, trust
//! auth, simple query ('Q'), Terminate ('X'). Extended-protocol messages get
//! a polite ErrorResponse so drivers fail loudly instead of hanging.

use std::sync::Arc;

use bytes::{BufMut, BytesMut};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::config::{Config, fnv64};
use crate::generate::generate;
use crate::infer::{self, SemanticType, WireType};
use crate::server::Shared;
use crate::shape::{self, ColumnSpec, CrushClass, ResultShape, StmtKind};

/// Rows flushed to the wire per write during a crush stream. Keeps server-side
/// memory flat no matter how many rows we promise the client.
const CRUSH_CHUNK: u64 = 1000;

/// A deliberately wide, type-diverse schema synthesised for `SELECT *` against
/// an unknown table — exercises many client codepaths at once.
static WIDE_CRUSH_COLUMNS: &[&str] = &[
    "id", "uuid", "name", "email", "phone", "status", "created_at", "updated_at", "price",
    "is_active", "description", "metadata",
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

        let mut out = BytesMut::with_capacity(8192);
        match tag {
            b'Q' => {
                let sql =
                    String::from_utf8_lossy(payload.strip_suffix(&[0]).unwrap_or(&payload)).into_owned();
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
            b'X' => return Ok(()),
            b'P' => {
                // extended query protocol: refuse clearly, then sync below
                put_error(
                    &mut out,
                    "0A000",
                    "EtherealDB speaks the simple query protocol only (so far); try psql, or simple_query in your driver",
                );
            }
            b'S' => put_ready(&mut out), // Sync
            b'H' => {}                   // Flush
            _ => {
                debug!(%peer, tag = (tag as char).to_string(), "ignoring message");
            }
        }
        if !out.is_empty() {
            stream.write_all(&out).await?;
        }
    }
}

/// A column resolved to everything needed to describe and fill it on the wire.
struct Resolved {
    name: String,
    st: SemanticType,
    wt: WireType,
    literal: Option<String>,
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

/// Append one DataRow ('D') filled with freshly generated values.
fn put_data_row(out: &mut BytesMut, cols: &[Resolved], rng: &mut impl Rng) {
    put_msg(out, b'D', |b| {
        b.put_i16(cols.len() as i16);
        for c in cols {
            let v = match &c.literal {
                Some(v) => v.clone(),
                None => generate(c.st, rng),
            };
            b.put_i32(v.len() as i32);
            b.put_slice(v.as_bytes());
        }
    });
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
                    put_notice(out, "crush mode at capacity — enjoy a normal answer instead");
                }
            }
        }
        CrushClass::Crush { .. } => {
            // warn-only: name and shame, then answer normally.
            warn!(%peer, reasons = class.reasons(), query = truncate(stmt), "unsafe query (warn-only)");
            put_notice(out, &format!("unsafe query ({}); crush mode is warn-only", class.reasons()));
        }
        CrushClass::Warn { .. } => {
            put_notice(
                out,
                &format!("loose query ({}) — consider a column list, WHERE, or LIMIT", class.reasons()),
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
            // like the real thing.
            let mut n = if shape.aggregate_only() || shape.table_hint.is_none() {
                1
            } else {
                rng.random_range(cfg.rows_min..=cfg.rows_max.max(cfg.rows_min))
            };
            if let Some(limit) = shape.limit {
                n = n.min(limit);
            }

            let cols: Vec<Resolved> = shape.columns.iter().map(resolve_column).collect();
            put_row_description(out, &cols);
            for _ in 0..n {
                put_data_row(out, &cols, &mut rng);
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

/// Stream an avalanche of type-correct rows until `max_rows` is reached or the
/// client stops reading. Uses O(1) memory: one chunk buffer, reused.
///
/// On success returns the row count sent. On write failure returns the count
/// sent so far plus the error (the client has almost certainly given up).
async fn crush_stream(
    stream: &mut TcpStream,
    shape: &ResultShape,
    cfg: &Config,
    stmt: &str,
) -> Result<u64, (u64, std::io::Error)> {
    let mut rng = seed_rng(cfg, stmt);

    // For `SELECT *` against an unknown table, synthesise a wide, diverse
    // schema; otherwise honour the columns the client actually asked for.
    let cols: Vec<Resolved> = if shape.select_star {
        WIDE_CRUSH_COLUMNS.iter().map(|n| resolve_named(n)).collect()
    } else {
        shape.columns.iter().map(resolve_column).collect()
    };

    let mut header = BytesMut::with_capacity(256);
    put_notice(
        &mut header,
        "CRUSH MODE: this query asked for everything — here it comes",
    );
    put_row_description(&mut header, &cols);
    if let Err(e) = stream.write_all(&header).await {
        return Err((0, e));
    }

    let max = cfg.crush.max_rows;
    let mut sent: u64 = 0;
    let mut chunk = BytesMut::with_capacity(64 * 1024);
    while sent < max {
        let this = CRUSH_CHUNK.min(max - sent);
        chunk.clear();
        for _ in 0..this {
            put_data_row(&mut chunk, &cols, &mut rng);
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
    let cut = (0..=MAX).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
    format!("{}…", &s[..cut])
}

/// Resolve a bare column name (no cast/literal) — used for synthesised schemas.
fn resolve_named(name: &str) -> Resolved {
    let st = infer::infer(name);
    Resolved { name: name.to_string(), st, wt: infer::wire_type(st), literal: None }
}

/// Resolve a column spec to everything needed to render it on the wire.
fn resolve_column(c: &ColumnSpec) -> Resolved {
    if let Some((value, wt)) = &c.literal {
        return Resolved {
            name: c.name.clone(),
            st: SemanticType::LoremShort,
            wt: *wt,
            literal: Some(value.clone()),
        };
    }
    let mut st = infer::infer(&c.name);
    let mut wt = infer::wire_type(st);
    if let Some(cast) = c.cast {
        // The cast wins the wire type; keep the name's flavor only when it
        // already agrees, otherwise generate a generic value of the cast type.
        if wt != cast {
            st = infer::generic_for(cast);
        }
        wt = cast;
    }
    Resolved { name: c.name.clone(), st, wt, literal: None }
}
