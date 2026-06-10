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
etherealdb infer email user_id ...  # ask the inference engine directly
```

Any username works. Any database name works. Trust auth, in the most
literal sense.

## What works (so far)

- Postgres simple query protocol: `psql`, and any driver's `simple_query`
  path.
- `SELECT` anything — columns are inferred by name, `SELECT *` conjures a
  default schema, `LIMIT` is honored, `count(*)` returns one row.
- Literals echo back (`SELECT 1` returns `1`), casts steer wire types
  (`x::int`), `SHOW server_version` answers politely.
- DML/DDL/transactions are cheerfully acknowledged and instantly forgotten.

See [PLAN.md](PLAN.md) for the roadmap: extended query protocol (ORMs),
MySQL, Redis, chaos gremlins.

## Development

```sh
cargo test     # unit tests + end-to-end against a real postgres client
cargo run      # start the server
```
