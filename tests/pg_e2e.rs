//! End-to-end: a real Postgres client (tokio-postgres) connects to
//! EtherealDB and runs queries over the simple-query protocol.

use std::sync::Arc;

use etherealdb::config::{Config, CrushConfig};
use etherealdb::server;
use tokio_postgres::{NoTls, SimpleQueryMessage};

async fn start_with(cfg: Config) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(server::serve(listener, Arc::new(cfg)));
    port
}

async fn start_server(seed: Option<u64>) -> u16 {
    start_with(Config { seed, ..Config::default() }).await
}

async fn connect(port: u16) -> tokio_postgres::Client {
    let (client, conn) = tokio_postgres::connect(
        &format!("host=127.0.0.1 port={port} user=ghost dbname=ethereal"),
        NoTls,
    )
    .await
    .expect("handshake should succeed");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

fn rows(msgs: &[SimpleQueryMessage]) -> Vec<&tokio_postgres::SimpleQueryRow> {
    msgs.iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(r) => Some(r),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn select_returns_plausible_rows() {
    let port = start_server(Some(42)).await;
    let client = connect(port).await;

    let msgs = client
        .simple_query("select id, email, is_active, created_at from users limit 7")
        .await
        .unwrap();
    let rows = rows(&msgs);
    assert!((1..=7).contains(&rows.len()), "got {} rows", rows.len());

    for row in rows {
        let id = row.get(0).unwrap();
        let email = row.get(1).unwrap();
        let active = row.get(2).unwrap();
        let created = row.get(3).unwrap();
        assert!(id.parse::<i64>().is_ok(), "id not an int: {id}");
        assert!(email.contains('@'), "email not an email: {email}");
        assert!(active == "t" || active == "f", "bad bool: {active}");
        assert!(created.len() == 19 && &created[4..5] == "-", "bad timestamp: {created}");
    }
}

#[tokio::test]
async fn count_star_returns_one_row() {
    let port = start_server(None).await;
    let client = connect(port).await;

    let msgs = client.simple_query("select count(*) from orders").await.unwrap();
    let rows = rows(&msgs);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get(0).unwrap().parse::<i64>().is_ok());
}

#[tokio::test]
async fn dml_and_commands_are_acked() {
    let port = start_server(None).await;
    let client = connect(port).await;

    let msgs = client
        .simple_query("insert into ghosts (name) values ('casper')")
        .await
        .unwrap();
    assert!(msgs.iter().any(|m| matches!(m, SimpleQueryMessage::CommandComplete(_))));

    client.simple_query("begin").await.unwrap();
    client.simple_query("create table phantom (id int)").await.unwrap();
    client.simple_query("commit").await.unwrap();
}

#[tokio::test]
async fn same_seed_same_garbage() {
    let port = start_server(Some(7)).await;
    let client = connect(port).await;

    let q = "select email, balance from accounts limit 3";
    let a = client.simple_query(q).await.unwrap();
    let b = client.simple_query(q).await.unwrap();

    let a: Vec<String> =
        rows(&a).iter().map(|r| format!("{}|{}", r.get(0).unwrap(), r.get(1).unwrap())).collect();
    let b: Vec<String> =
        rows(&b).iter().map(|r| format!("{}|{}", r.get(0).unwrap(), r.get(1).unwrap())).collect();
    assert_eq!(a, b, "deterministic mode should repeat itself");
}

#[tokio::test]
async fn select_one_echoes_literal() {
    let port = start_server(None).await;
    let client = connect(port).await;

    let msgs = client.simple_query("select 1").await.unwrap();
    let rows = rows(&msgs);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0).unwrap(), "1");
}

fn crush_config(max_rows: u64) -> Config {
    Config {
        seed: Some(99),
        crush: CrushConfig { enabled: true, max_rows, ..CrushConfig::default() },
        ..Config::default()
    }
}

#[tokio::test]
async fn unsafe_query_is_crushed() {
    // Keep max_rows modest so the test stays fast but still spans many chunks.
    let port = start_with(crush_config(25_000)).await;
    let client = connect(port).await;

    let msgs = client.simple_query("select * from users").await.unwrap();
    let rows = rows(&msgs);
    assert_eq!(rows.len(), 25_000, "crush should stream exactly max_rows");

    // Wide synthesised schema: still type-correct under the avalanche.
    let r = &rows[0];
    assert!(r.get(0).unwrap().parse::<i64>().is_ok(), "id should be an int");
    let email_col = (0..r.len()).find(|&i| msgs_col_name(&msgs, i) == Some("email"));
    if let Some(i) = email_col {
        assert!(r.get(i).unwrap().contains('@'));
    }
}

/// Find the column index name from the first row's statement, if exposed.
fn msgs_col_name(msgs: &[SimpleQueryMessage], idx: usize) -> Option<&str> {
    msgs.iter().find_map(|m| match m {
        SimpleQueryMessage::Row(r) => r.columns().get(idx).map(|c| c.name()),
        _ => None,
    })
}

#[tokio::test]
async fn safe_query_is_spared_under_crush() {
    let port = start_with(crush_config(1_000_000)).await;
    let client = connect(port).await;

    // Explicit columns + WHERE + LIMIT — restraint, so a normal small result.
    let msgs = client
        .simple_query("select id, email from users where id = 5 limit 8")
        .await
        .unwrap();
    let safe = rows(&msgs);
    assert!(safe.len() <= 8, "safe query must not be crushed, got {}", safe.len());

    // count(*) is always safe even though it has no WHERE/LIMIT/columns.
    let msgs = client.simple_query("select count(*) from users").await.unwrap();
    assert_eq!(rows(&msgs).len(), 1);
}

#[tokio::test]
async fn warn_only_does_not_crush() {
    let cfg = Config {
        crush: CrushConfig {
            enabled: true,
            warn_only: true,
            max_rows: 1_000_000,
            ..CrushConfig::default()
        },
        ..Config::default()
    };
    let port = start_with(cfg).await;
    let client = connect(port).await;

    // Unsafe query, but warn-only: should get a normal (small) result, not 1M rows.
    let msgs = client.simple_query("select * from users").await.unwrap();
    assert!(rows(&msgs).len() <= 20, "warn-only must answer normally");
}
