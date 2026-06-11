# EtherealDB

> A database that isn't there.

EtherealDB speaks the PostgreSQL wire protocol, accepts **any** query, and
returns random-but-plausible nonsense. Column values are chosen by a
lightweight inference engine that guesses a column's semantic type from its
name: `email` gets email addresses, `created_at` gets timestamps, `price`
gets decimals, `is_active` gets booleans.

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
etherealdb --seed 42                # deterministic: same query, same garbage
etherealdb --rows 100:500           # row-count band when there's no LIMIT
etherealdb --crush                  # crush mode (see below)
etherealdb infer email user_id ...  # ask the inference engine directly
```

Any username works. Any database name works. Trust auth, in the most
literal sense.

## What works (so far)

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
- DML/DDL/transactions are cheerfully acknowledged and instantly forgotten.
- **Crush mode** (`--crush`): unsafe queries trigger a streamed avalanche of
  rows to overload careless clients. See below.

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

See [PLAN.md](PLAN.md) for the roadmap: GUI-client introspection stubs
(DBeaver/TablePlus), MySQL, Redis.

## Development

```sh
cargo test     # unit tests + end-to-end against a real postgres client
cargo run      # start the server
```
