//! End-to-end: a real Postgres client (tokio-postgres) connects to
//! EtherealDB and runs queries over the simple-query protocol.

use std::sync::Arc;

use etherealdb::config::{Config, CrushConfig};
use etherealdb::infer::Rules;
use etherealdb::server;
use etherealdb::theme;
use tokio_postgres::{NoTls, SimpleQueryMessage};

async fn start_with(cfg: Config) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(server::serve(listener, Arc::new(cfg)));
    port
}

async fn start_server(seed: Option<u64>) -> u16 {
    start_with(Config {
        seed,
        ..Config::default()
    })
    .await
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
        assert!(
            created.len() == 19 && &created[4..5] == "-",
            "bad timestamp: {created}"
        );
    }
}

#[tokio::test]
async fn count_star_returns_one_row() {
    let port = start_server(None).await;
    let client = connect(port).await;

    let msgs = client
        .simple_query("select count(*) from orders")
        .await
        .unwrap();
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
    assert!(
        msgs.iter()
            .any(|m| matches!(m, SimpleQueryMessage::CommandComplete(_)))
    );

    client.simple_query("begin").await.unwrap();
    client
        .simple_query("create table phantom (id int)")
        .await
        .unwrap();
    client.simple_query("commit").await.unwrap();
}

#[tokio::test]
async fn same_seed_same_garbage() {
    let port = start_server(Some(7)).await;
    let client = connect(port).await;

    let q = "select email, balance from accounts limit 3";
    let a = client.simple_query(q).await.unwrap();
    let b = client.simple_query(q).await.unwrap();

    let a: Vec<String> = rows(&a)
        .iter()
        .map(|r| format!("{}|{}", r.get(0).unwrap(), r.get(1).unwrap()))
        .collect();
    let b: Vec<String> = rows(&b)
        .iter()
        .map(|r| format!("{}|{}", r.get(0).unwrap(), r.get(1).unwrap()))
        .collect();
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
        crush: CrushConfig {
            enabled: true,
            max_rows,
            ..CrushConfig::default()
        },
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
    assert!(
        r.get(0).unwrap().parse::<i64>().is_ok(),
        "id should be an int"
    );
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
    assert!(
        safe.len() <= 8,
        "safe query must not be crushed, got {}",
        safe.len()
    );

    // count(*) is always safe even though it has no WHERE/LIMIT/columns.
    let msgs = client
        .simple_query("select count(*) from users")
        .await
        .unwrap();
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

// ---- Extended query protocol (client.query / prepared statements) ----

#[tokio::test]
async fn extended_query_typed_binary_decode() {
    let port = start_server(Some(123)).await;
    let client = connect(port).await;

    // client.query uses the extended protocol with binary result formats; the
    // values must decode into their native Rust types.
    let rows = client
        .query(
            "select id, email, is_active, created_at, price from accounts limit 6",
            &[],
        )
        .await
        .unwrap();
    assert!(!rows.is_empty() && rows.len() <= 6);

    for row in &rows {
        let _id: i64 = row.get(0);
        let email: &str = row.get(1);
        let _active: bool = row.get(2);
        let _created: std::time::SystemTime = row.get(3);
        let price: f64 = row.get(4); // numeric is advertised as float8
        assert!(email.contains('@'));
        assert!(price >= 0.0);
    }
}

#[tokio::test]
async fn extended_handles_int_and_uuid_and_date() {
    let port = start_server(Some(5)).await;
    let client = connect(port).await;

    let rows = client
        .query(
            "select user_id, account_uuid, signup_date from members limit 4",
            &[],
        )
        .await
        .unwrap();
    assert!(!rows.is_empty());
    for row in &rows {
        let _fk: i64 = row.get(0);
        let _uuid: uuid::Uuid = row.get(1);
        let _date: chrono::NaiveDate = row.get(2);
    }
}

#[tokio::test]
async fn prepared_statement_with_param() {
    let port = start_server(Some(77)).await;
    let client = connect(port).await;

    // `id = $1` — the inference engine reports the param as int8 (it's an `id`),
    // so binding an i64 type-checks against ParameterDescription.
    let stmt = client
        .prepare("select id, email from users where id = $1 limit 3")
        .await
        .unwrap();
    assert_eq!(stmt.params(), &[tokio_postgres::types::Type::INT8]);
    let rows = client.query(&stmt, &[&42i64]).await.unwrap();
    assert!(!rows.is_empty() && rows.len() <= 3);
    let _id: i64 = rows[0].get(0);
}

#[tokio::test]
async fn extended_select_literal() {
    let port = start_server(None).await;
    let client = connect(port).await;

    let row = client.query_one("select 1 as n", &[]).await.unwrap();
    let n: i32 = row.get(0);
    assert_eq!(n, 1);
}

#[tokio::test]
async fn extended_crush_streams_many_rows() {
    let port = start_with(crush_config(15_000)).await;
    let client = connect(port).await;

    // Unsafe query over the extended protocol still triggers the avalanche.
    let rows = client.query("select * from users", &[]).await.unwrap();
    assert_eq!(rows.len(), 15_000);
    let _id: i64 = rows[0].get(0);
}

// ---- GUI-client introspection stubs ----

#[tokio::test]
async fn introspection_functions_answer_believably() {
    let port = start_server(None).await;
    let client = connect(port).await;

    let row = client.query_one("select version()", &[]).await.unwrap();
    let v: &str = row.get(0);
    assert!(v.starts_with("PostgreSQL"), "version() = {v}");

    let row = client
        .query_one("select current_database()", &[])
        .await
        .unwrap();
    let db: &str = row.get(0);
    assert_eq!(db, "ethereal");

    let row = client.query_one("select current_user", &[]).await.unwrap();
    let u: &str = row.get(0);
    assert_eq!(u, "ghost");
}

#[tokio::test]
async fn catalog_queries_return_empty() {
    let port = start_server(None).await;
    let client = connect(port).await;

    // GUI introspection should see an empty database, not garbage rows.
    let rows = client
        .query("select typname from pg_catalog.pg_type", &[])
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "pg_type should be empty, got {}",
        rows.len()
    );

    let rows = client
        .query("select table_name from information_schema.tables", &[])
        .await
        .unwrap();
    assert!(rows.is_empty(), "information_schema.tables should be empty");
}

#[tokio::test]
async fn catalog_query_is_not_crushed() {
    // Even with an aggressive threshold, a `select *` against a catalog must
    // never be crushed — that's how GUIs connect.
    let cfg = Config {
        crush: CrushConfig {
            enabled: true,
            threshold: etherealdb::shape::CrushThreshold::Star,
            max_rows: 10_000_000,
            ..CrushConfig::default()
        },
        ..Config::default()
    };
    let port = start_with(cfg).await;
    let client = connect(port).await;

    let rows = client
        .query("select * from pg_catalog.pg_class", &[])
        .await
        .unwrap();
    assert!(rows.is_empty(), "catalog query must be empty, not crushed");
}

// ---- Themes & custom inference rules ----

#[tokio::test]
async fn theme_changes_status_vocabulary() {
    let cfg = Config {
        theme: theme::by_name("ecommerce").unwrap(),
        ..Config::default()
    };
    let port = start_with(cfg).await;
    let client = connect(port).await;

    let msgs = client
        .simple_query("select status from orders limit 12")
        .await
        .unwrap();
    let eco = theme::by_name("ecommerce").unwrap();
    for r in rows(&msgs) {
        let v = r.get(0).unwrap();
        assert!(eco.statuses.contains(&v), "{v} is not an ecommerce status");
    }
}

#[tokio::test]
async fn custom_rules_override_inference() {
    // Without rules, `coupon` is just lorem text; the rule makes it a short code.
    let cfg = Config {
        rules: Rules::parse("exact coupon short_code").unwrap(),
        ..Config::default()
    };
    let port = start_with(cfg).await;
    let client = connect(port).await;

    let msgs = client
        .simple_query("select coupon from cart limit 6")
        .await
        .unwrap();
    for r in rows(&msgs) {
        let v = r.get(0).unwrap();
        assert_eq!(v.len(), 8, "short code should be 8 chars: {v}");
        assert!(
            v.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "short code should be uppercase alnum: {v}"
        );
    }
}

// ---- Ghosts: fault injection & fuzzing ----

use etherealdb::config::GhostConfig;

#[tokio::test]
async fn ghost_error_makes_query_fail() {
    let cfg = Config {
        ghosts: GhostConfig {
            error_prob: 1.0,
            ..GhostConfig::default()
        },
        ..Config::default()
    };
    let port = start_with(cfg).await;
    let client = connect(port).await;

    // Connect succeeds (ghosts only haunt queries); the query gets an error.
    let err = client
        .simple_query("select id from users")
        .await
        .unwrap_err();
    let db = err.as_db_error().expect("should be a server error");
    assert!(
        db.message().contains("ghost"),
        "expected a ghostly error: {}",
        db.message()
    );
}

#[tokio::test]
async fn ghost_drop_kills_the_connection() {
    let cfg = Config {
        ghosts: GhostConfig {
            drop_prob: 1.0,
            ..GhostConfig::default()
        },
        ..Config::default()
    };
    let port = start_with(cfg).await;
    let client = connect(port).await;

    // The server drops the connection rather than answering.
    assert!(client.simple_query("select 1").await.is_err());
}

#[tokio::test]
async fn fuzz_emits_pathological_values_without_crashing() {
    let cfg = Config {
        seed: Some(1),
        ghosts: GhostConfig {
            fuzz: 1.0,
            ..GhostConfig::default()
        },
        ..Config::default()
    };
    let port = start_with(cfg).await;
    let client = connect(port).await;

    // Every value is a ghost value; the server must stay up and keep framing
    // correct, and an `id` column should NOT always be a clean integer.
    let msgs = client
        .simple_query("select id from accounts limit 12")
        .await
        .unwrap();
    let rows = rows(&msgs);
    assert!(!rows.is_empty());
    let all_clean_ints = rows
        .iter()
        .all(|r| r.get(0).unwrap().parse::<i64>().is_ok());
    assert!(
        !all_clean_ints,
        "fuzz should produce some non-integer id values"
    );
}
