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
use tracing::{debug, info};

use crate::config::{Config, fnv64};
use crate::generate::generate;
use crate::infer::{self, SemanticType, WireType};
use crate::shape::{self, ColumnSpec, StmtKind};

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

pub async fn handle(mut stream: TcpStream, cfg: Arc<Config>) -> std::io::Result<()> {
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
                let sql = String::from_utf8_lossy(payload.strip_suffix(&[0]).unwrap_or(&payload));
                debug!(%peer, %sql, "query");
                let stmts = shape::split_statements(&sql);
                if stmts.is_empty() {
                    put_msg(&mut out, b'I', |_| {}); // EmptyQueryResponse
                } else {
                    for stmt in stmts {
                        respond_statement(&mut out, stmt, &cfg);
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

fn respond_statement(out: &mut BytesMut, stmt: &str, cfg: &Config) {
    let shape = shape::extract(stmt);
    let mut rng = match cfg.seed {
        Some(seed) => ChaCha8Rng::seed_from_u64(seed ^ fnv64(stmt.as_bytes())),
        None => ChaCha8Rng::from_os_rng(),
    };

    match shape.kind {
        StmtKind::Empty => put_msg(out, b'I', |_| {}),
        StmtKind::Select => {
            let aggregate_only =
                !shape.columns.is_empty() && shape.columns.iter().all(|c| c.aggregate);
            // No FROM clause (SELECT 1, SELECT now()) yields exactly one row,
            // like the real thing.
            let mut n = if aggregate_only || shape.table_hint.is_none() {
                1
            } else {
                rng.random_range(cfg.rows_min..=cfg.rows_max.max(cfg.rows_min))
            };
            if let Some(limit) = shape.limit {
                n = n.min(limit);
            }

            let resolved: Vec<(String, SemanticType, WireType, Option<String>)> = shape
                .columns
                .iter()
                .map(|c| resolve_column(c))
                .collect();

            put_msg(out, b'T', |b| {
                b.put_i16(resolved.len() as i16);
                for (name, _, wt, _) in &resolved {
                    put_cstr(b, name);
                    b.put_i32(0); // table oid
                    b.put_i16(0); // attnum
                    b.put_u32(oid(*wt));
                    b.put_i16(-1); // typlen
                    b.put_i32(-1); // typmod
                    b.put_i16(0); // text format
                }
            });
            for _ in 0..n {
                put_msg(out, b'D', |b| {
                    b.put_i16(resolved.len() as i16);
                    for (_, st, _, literal) in &resolved {
                        let v = match literal {
                            Some(v) => v.clone(),
                            None => generate(*st, &mut rng),
                        };
                        b.put_i32(v.len() as i32);
                        b.put_slice(v.as_bytes());
                    }
                });
            }
            put_command_complete(out, &format!("SELECT {n}"));
        }
        StmtKind::Insert => {
            put_command_complete(out, "INSERT 0 1");
        }
        StmtKind::Update => {
            let n: u32 = rng.random_range(0..50);
            put_command_complete(out, &format!("UPDATE {n}"));
        }
        StmtKind::Delete => {
            let n: u32 = rng.random_range(0..50);
            put_command_complete(out, &format!("DELETE {n}"));
        }
        StmtKind::Command(tag) => put_command_complete(out, &tag),
    }
}

/// Resolve a column spec to (name, generator, wire type, literal override).
fn resolve_column(c: &ColumnSpec) -> (String, SemanticType, WireType, Option<String>) {
    if let Some((value, wt)) = &c.literal {
        return (c.name.clone(), SemanticType::LoremShort, *wt, Some(value.clone()));
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
    (c.name.clone(), st, wt, None)
}
