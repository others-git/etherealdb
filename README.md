<p align="center">
  <img src="assets/etherealdb.svg" width="140" height="140" alt="EtherealDB logo">
</p>

<h1 align="center">EtherealDB</h1>

<p align="center"><em>A database that isn't there.</em></p>

<p align="center">
  <a href="https://github.com/others-git/etherealdb/actions/workflows/ci.yml"><img src="https://github.com/others-git/etherealdb/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/built%20with-Rust-dea584?logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/protocols-PostgreSQL%20%7C%20MySQL%20%7C%20Redis-a78bfa" alt="Protocols">
  <img src="https://img.shields.io/badge/license-MIT-7dd3fc" alt="License">
</p>

EtherealDB speaks the PostgreSQL, MySQL, and Redis wire protocols, accepts
**any** query, and returns random-but-plausible nonsense. Values are chosen by a
lightweight inference engine that guesses a column's (or key's) semantic type
from its name: `email` gets email addresses, `created_at` gets timestamps,
`price` gets decimals, `is_active` gets booleans.

No schema. No storage. No truth. Every database exists, every query succeeds.

```
$ etherealdb &
$ psql -h 127.0.0.1 -U anyone any_database_at_all

NOTICE:  welcome to EtherealDB — every query succeeds, no data is real

ethereal=> select id, email, is_active, created_at from users limit 3;
   id   |            email             | is_active |     created_at
--------+------------------------------+-----------+---------------------
 482113 | priya.tanaka@nimbus.net      | t         | 2025-11-02 14:22:08
 90210  | sven.almeida@quarkmail.com   | f         | 2024-08-19 03:41:55
 7741   | imani.dubois@fathom.app      | t         | 2026-01-30 21:09:12
```

## Why

Demos, screenshots, teaching, fuzzing ORMs, load-testing clients without
standing up real data — and entertainment.

## Usage

```sh
etherealdb                          # postgres protocol on 127.0.0.1:5432
etherealdb --pg 0.0.0.0:5433        # different bind address
etherealdb --mysql 127.0.0.1:3306   # also speak MySQL (off by default)
etherealdb --redis 127.0.0.1:6379    # also speak Redis/RESP (off by default)
etherealdb --seed 42                # deterministic: same query, same garbage
etherealdb --rows 100:500           # row-count band when there's no LIMIT
etherealdb --theme ecommerce        # domain-flavored values (see below)
etherealdb --rules my-rules.txt     # custom inference rules (see below)
etherealdb --crush                  # crush mode (see below)
etherealdb --ghost-errors 0.2       # haunt 20% of queries with errors (see below)
etherealdb --fuzz 0.1               # 10% of values are pathological junk
etherealdb infer email user_id ...  # ask the inference engine directly
```

Run all protocols at once and connect with `psql`, `mysql`, or `redis-cli` —
same inference, themes, and crush mode behind every one.

Any username works. Any database name works. Trust auth, in the most
literal sense.

## What works (so far)

- **MySQL** protocol (`--mysql`): handshake + trust auth, `COM_QUERY` text
  result sets, so the `mysql` CLI and drivers connect. Crush mode and catalog
  stubs work here too — the whole engine is shared with Postgres.
- **Redis** protocol (`--redis`): RESP2, so `redis-cli` and drivers connect.
  Here the value type is inferred from the *key* — `GET user:42:email` returns
  an email, `GET cart:9:total` returns money. `GET`/`SET`/`MGET`/`HGETALL`/
  `KEYS`/`SCAN`/`INCR`/`TTL`/`TYPE`/`EXISTS` and more are answered; `KEYS *` is
  the crush trigger (the Redis analogue of `SELECT *`).
- Postgres **simple** query protocol: `psql`, and any driver's `simple_query`
  path.
- Postgres **extended** query protocol: Parse/Bind/Describe/Execute, so drivers
  and ORMs using `client.query(...)` / prepared statements connect. Results are
  encoded in **binary** when the client asks (int/bool/float/text/json/uuid/
  date/time/timestamp). Parameter types are inferred from context — `where id =
  $1` reports an int param, `where email = $2` a text one.
- `SELECT` anything — columns are inferred by name, `SELECT *` conjures a
  default schema, `LIMIT` is honored, `count(*)` returns one row.
- Literals echo back (`SELECT 1` returns `1`), casts steer wire types
  (`x::int`), `SHOW server_version` answers politely.
- **GUI-friendly**: `version()`, `current_database()`, `current_user` and
  friends answer believably, and `pg_catalog`/`information_schema` queries
  return *empty* result sets — so DBeaver/TablePlus/pgAdmin connect and show an
  empty database instead of choking on fake catalog rows.
- DML/DDL/transactions are cheerfully acknowledged and instantly forgotten.
- **Crush mode** (`--crush`): unsafe queries trigger a streamed avalanche of
  rows to overload careless clients. See below.
- **Ghosts** (`--ghost-*`, `--fuzz`): inject latency, errors, dropped
  connections, and pathological values to test client resilience. See below.
- **Themes & custom rules** (`--theme`, `--rules`): domain-flavored values and
  user-defined inference. See below.

## Crush mode

`--crush` turns EtherealDB into a client stress-tester. A query that shows
restraint — a specific column list, a `WHERE`, a `LIMIT` — gets a normal
answer. A query that would dump a whole table in production (`SELECT * FROM x`
with no `WHERE` and no `LIMIT`) springs the trap: a torrent of type-correct
rows, streamed in chunks until the client's memory, buffer, or patience gives
out. Point a BI tool or ORM at it and find out whether the client paginates
like it should.

```sh
etherealdb --crush                       # arm it (default: all-three-signals → crush)
etherealdb --crush --crush-rows 50000000 # how many rows an unsafe query earns
etherealdb --crush --crush-warn-only     # log unsafe queries, but answer normally
etherealdb --crush --crush-threshold any2  # crush when any two signals are missing
etherealdb --crush --crush-concurrency 8  # cap simultaneous crush streams
```

```
$ psql -h 127.0.0.1 -c 'select * from users'
NOTICE:  CRUSH MODE: this query asked for everything — here it comes
... 5,000,000 rows later ...
```

`count(*)`, `SELECT 1`, `SHOW`, DDL, and DML are always safe — crush only fires
on table reads with no row budget. The server itself streams in O(1) memory; the
client is on its own.

See [PLAN.md](PLAN.md) for the roadmap: themes, custom inference rules, Redis.

## Ghosts: fault injection & fuzzing

Real databases are flaky. **Ghosts** haunt the server so you can test whether
your client survives a half-real backend — latency spikes, the odd error,
dropped connections, and corrupt values — across *all three* protocols.

```sh
etherealdb --ghost-latency 0.3 --ghost-latency-ms 50:800  # 30% of queries lag
etherealdb --ghost-errors 0.15    # 15% answered with a protocol error
etherealdb --ghost-drops 0.05     # 5% drop the connection mid-conversation
etherealdb --fuzz 0.2             # 20% of values are pathological junk
```

`--fuzz` makes EtherealDB a **client fuzzer**: per value, it emits something
designed to break naive decoders — empty strings, 16 KB blobs, `NaN`,
`-Infinity`, `2026-13-45`, embedded NULs, RTL/emoji/unicode, huge integers —
chosen to antagonise that column's declared type. Great for finding where a
driver or ORM assumes the database always behaves.

```
$ etherealdb infer id email created_at --fuzz 1.0
id           IdInt        99999999999999999999999999 | -Infinity | AAAA…(4 KB)
email        Email        Ω≈ç√∫˜µ≤≥÷ | \x00\x01\x02 | NaN
created_at   Timestamp    9999-99-99 99:99:99 |  | not-a-date
```

Ghost decisions use a separate RNG, so `--seed` still gives deterministic
*data* while faults stay random. All knobs default to off.

## Themes & custom rules

**`--theme`** swaps the vocabulary the generators draw from, so values feel like
they belong to a domain. Built-ins: `generic` (default), `ecommerce`, `finance`,
`iot`, `users`. A `status` column reads `active`/`archived` under `generic` but
`shipped`/`refunded` under `ecommerce`; `type`/`kind` columns and free text shift
to match too.

```
$ etherealdb infer status order_type --theme ecommerce
status       StatusEnum   shipped | refunded | pending
order_type   KindEnum     physical | giftcard | digital
```

**`--rules <file>`** layers your own name→type rules over the built-in inference
engine (yours win). The format is one rule per line — `<kind> <pattern> <type>`,
where kind is `exact`/`suffix`/`prefix`/`token` — no config language to learn:

```text
exact   coupon_code   short_code
suffix  _balance      money
prefix  flag_         bool
token   gateway       ip
```

See [`examples/rules.example.txt`](examples/rules.example.txt). Both flags work
on the live server and on the `infer` debug subcommand.

## Docker

The image is a ~10 MB static binary on Alpine, listening on both protocols:

```sh
docker build -t etherealdb .
docker run --rm -p 5432:5432 -p 3306:3306 etherealdb

# pass flags through (they override the default CMD):
docker run --rm -p 5432:5432 etherealdb --pg 0.0.0.0:5432 --seed 42 --crush
```

Released images are published to GHCR on every tag
(`ghcr.io/others-git/etherealdb`).

## Development

```sh
cargo test            # unit tests + e2e against real postgres & mysql clients
cargo run             # start the server
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

CI runs fmt, clippy, the full test suite, and a Docker build on every push;
tagging `vX.Y.Z` builds release binaries (x86_64 + aarch64 musl) and pushes a
multi-arch image.
