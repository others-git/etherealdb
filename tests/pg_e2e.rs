//! End-to-end: a real Postgres client (tokio-postgres) connects to
//! EtherealDB and runs queries over the simple-query protocol.

use std::sync::Arc;

use etherealdb::config::Config;
use etherealdb::server;
use tokio_postgres::{NoTls, SimpleQueryMessage};

async fn start_server(seed: Option<u64>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cfg = Arc::new(Config { seed, rows_min: 5, rows_max: 20 });
    tokio::spawn(server::serve(listener, cfg));
    port
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
