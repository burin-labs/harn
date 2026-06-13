# Postgres

`std/postgres` exposes VM-native Postgres helpers for Harn pipelines that need
tenant state, receipts, event logs, claims, audit records, or other durable
relational state.

```harn
import "std/postgres"

pipeline default() {
  let db = pg_pool("env:DATABASE_URL", {
    max_connections: 5,
    acquire_timeout_ms: 3000,
    ssl_mode: "require",
    application_name: "harn-harness",
  })

  let rows = pg_query(
    db,
    "select id, payload, created_at from receipts where tenant_id = $1 order by created_at desc",
    ["tenant-123"],
  )

  log(json_stringify(rows))
  pg_close(db)
}
```

## Functions

| Function | Returns | Notes |
|---|---|---|
| `pg_pool(source, options?)` | `PgPool` | Open a pooled Postgres connection. |
| `pg_connect(source, options?)` | `PgPool` | Open a single-connection pool, useful for session-oriented work. |
| `pg_query(handle, sql, params?)` | `list<dict>` | Run a parameterized query and return decoded rows. |
| `pg_query_one(handle, sql, params?)` | `dict` or `nil` | Return the first row, or `nil` when the query returns no rows. |
| `pg_execute(handle, sql, params?)` | `PgExecuteResult` | Run a statement that does not need returned rows. Returns `{rows_affected, duration_ms}`. |
| `pg_transaction(pool, fn(tx) -> any, options?)` | closure result | Begin a transaction, pass a scoped `PgTx` handle to the closure, commit on normal return, rollback when the closure throws. |
| `pg_savepoint(tx, name)` | `bool` | Create a savepoint inside an open transaction. |
| `pg_release_savepoint(tx, name)` | `bool` | Release a previously created savepoint. |
| `pg_rollback_to_savepoint(tx, name)` | `bool` | Roll work back to a savepoint while keeping the outer transaction open. |
| `pg_migrate(pool, {dir, ledger?, table?, dry_run?})` | `PgMigrateResult` | Apply `.sql` files from a directory; track the applied set in `harn_migrations` (override via `table`). Pass `ledger: "sqlx"` to read/write SQLx's `_sqlx_migrations` ledger instead. |
| `pg_close(pool)` | `bool` | Close and unregister a pool handle. |
| `pg_stmt_cache_clear(pool)` | `PgStmtCacheClearResult` | Clear prepared-statement caches on idle primary and replica connections without closing the pool. |
| `pg.jsonb.path(pool, document, jsonpath)` | `list<any>` | Run `jsonb_path_query($1::jsonb, $2::jsonpath)` with bound operands. |
| `pg.jsonb.merge(pool, left, right)` | `any` | Run Postgres JSONB merge (`$1::jsonb \|\| $2::jsonb`). |
| `pg.jsonb.contains(pool, left, right)` | `bool` | Run Postgres JSONB containment (`$1::jsonb @> $2::jsonb`). |
| `pg_mock_pool(fixtures)` | `PgMockPool` | Create an in-process fixture-backed pool for tests. |
| `pg_mock_calls(mock)` | `list<dict>` | Inspect SQL, params, and execute/query mode recorded by a mock pool. |

`source` may be a raw Postgres URL, `env:VARIABLE_NAME`, `secret:namespace/name`,
or a dict with one of `url`, `env`, or `secret`. `secret:` references use the
active Harn connector secret context, so they are available while executing a
Harn-backed connector export.

Pool options include `max_connections`, `min_connections`,
`acquire_timeout_ms`, `timeout_ms`, `idle_timeout_ms`, `max_lifetime_ms`,
`ssl_mode` or `tls_mode`, `application_name`, and
`statement_cache_capacity`. Pool options also accept `replicas` and
`read_routing_policy` (`primary`, `replica`, `replica_or_primary`,
`round_robin_replica`). Prepared statement caching is driver-managed by SQLx;
tune it with `statement_cache_capacity` when needed. After a migration changes
result column types, call `pg_stmt_cache_clear(pool)` to evict cached prepared
statements from currently idle primary and replica connections. Connections
checked out by in-flight queries are left alone and counted in
`connections_skipped`, so callers that need a full sweep can retry once work has
drained.

## Parameters and decoding

Always pass dynamic values through the `params` list. Harn values are bound as
Postgres parameters rather than interpolated into SQL:

```harn
let receipt = pg_query_one(
  db,
  "select id, payload from receipts where tenant_id = $1 and id = $2::uuid",
  [tenant_id, receipt_id],
)
```

Primitive Harn values bind as booleans, integers, floats, text, bytea, or null.
Lists, dicts, structs, sets, and other compound values bind as JSON.

Rows decode into dictionaries keyed by column name. Built-in decoding covers
nulls, booleans, integer and float types, text, `uuid`, `json`/`jsonb`, `bytea`,
`date`, `time`, `timestamp`, `timestamptz`, common arrays, `hstore`, range
types, and Postgres geometric types. Unknown types are decoded as text when the
Postgres driver can expose them that way.

## Query ergonomics

`std/postgres/query` is a small Harn-native layer over the raw `pg_*`
builtins. It is not an ORM: SQL stays visible, Postgres-specific casts and
operators stay available, and every dynamic value still goes through Postgres
bind parameters.
The helpers are intended for data-access modules where long inline SQL calls
make reviews noisy.

```harn,ignore
import "std/postgres"
import {
  ident,
  many,
  named_sql,
  nullable_timestamptz_json,
  run,
  sql,
  uuid_text,
} from "std/postgres/query"

pub fn list_receipts_query(tenant_id: string, limit: int) {
  return named_sql(
    "list_receipts",
    "many",
    """
SELECT {id}, {tenant_id_column}, payload, {finished_at}
FROM {table}
WHERE tenant_id = {tenant_id}::uuid
ORDER BY {created_at} DESC
LIMIT {limit}
""",
    {
      id: uuid_text("id"),
      tenant_id_column: uuid_text("tenant_id"),
      finished_at: nullable_timestamptz_json("finished_at"),
      table: ident("receipts"),
      tenant_id: tenant_id,
      created_at: ident("created_at"),
      limit: limit,
    },
    {read_only: true},
  )
}

pub fn list_receipts(db, tenant_id: string) {
  return run(db, list_receipts_query(tenant_id, 50))
}
```

`sql(template, values?)` and `named_sql(name, mode, template, values?)` replace
ordinary `{name}` placeholders with `$1`, `$2`, ... and store the Harn values
in `params`. If the same placeholder appears more than once, it reuses the
first parameter index. Use `{{` and `}}` for literal braces:

```harn,ignore
let q = sql(
  "SELECT '{{}}' AS empty_json, {tenant_id}::uuid AS tenant_id, {tenant_id}::uuid AS again",
  {tenant_id: tenant_id},
)
// q.sql    == "SELECT '{}' AS empty_json, $1::uuid AS tenant_id, $1::uuid AS again"
// q.params == [tenant_id]
```

For brace-heavy literal SQL — JSON paths (`#>>'{}'`), array literals
(`'{a,b}'::text[]`), `jsonb_set(.., '{path}', ..)` — doubling every brace is
noisy and error-prone. `raw_sql(template, params?)` and
`named_raw_sql(name, mode, template, params?)` take the SQL byte-for-byte with
**no** `{name}` scanning, so braces need no `{{`/`}}` escaping. Parameters are
positional — write `$1`, `$2`, ... yourself and pass the matching list:

```harn,ignore
let q = raw_sql(
  "SELECT data#>>'{}' AS data FROM events WHERE tags && '{a,b}'::text[] AND tenant_id = $1::uuid",
  [tenant_id],
)
// q.sql    == "SELECT data#>>'{}' AS data FROM events WHERE tags && '{a,b}'::text[] AND tenant_id = $1::uuid"
// q.params == [tenant_id]
```

Use `sql(...)` / `named_sql(...)` when you want named placeholders and
`{{`/`}}` escaping; reach for `raw_sql(...)` / `named_raw_sql(...)` when the SQL
is literal and the braces are the SQL's own.

Postgres cannot bind identifiers as parameters. Use `ident(...)` or
`ident_path(...)` when SQL structure must be dynamic, and reserve
`unsafe_sql(...)` for the rare source-controlled fragment that no typed helper
covers:

```harn,ignore
let q = sql(
  "SELECT {column} FROM {table} WHERE tenant_id = {tenant_id}",
  {column: ident("created_at"), table: ident_path(["app", "receipts"]), tenant_id: tenant_id},
)
```

For one-off call sites, pass the template query record directly:

```harn,ignore
let rows = many(db, sql(
  "SELECT id::text AS id, payload FROM receipts WHERE tenant_id = {tenant_id}::uuid",
  {tenant_id: tenant_id},
))
```

Compared with direct `pg_query`, named query records make the SQL and params
easy to inspect in tests while keeping execution explicit:

```harn,ignore
let db = pg_mock_pool([
  {
    sql: "SELECT id::text AS id, payload FROM receipts WHERE tenant_id = $1::uuid",
    params: ["tenant-a"],
    rows: [{id: "r1", payload: {ok: true}}],
  },
])

let rows = run(db, named(
  "list_receipts",
  "many",
  "SELECT id::text AS id, payload FROM receipts WHERE tenant_id = $1::uuid",
  ["tenant-a"],
))
assert_eq(rows[0].id, "r1")
assert_eq(pg_mock_calls(db)[0].params[0], "tenant-a")
```

`one(handle, query)`, `many(handle, query)`, and `exec(handle, query)` force the
matching mode when a record includes `mode`. `run(handle, named_query)` dispatches
from the record's `mode`. When a named query fails, the thrown error includes
the query name before the underlying Postgres or mock-pool error. Query records
may include `options` for read routing; `one(...)` and `many(...)` pass those
through to `pg_query_one` and `pg_query`.

Projection helpers accept static SQL identifiers matching
`[A-Za-z_][A-Za-z0-9_]*`, optionally table-qualified (`vaults.created_at`) —
every dot-separated segment is validated, and the output alias is the trailing
segment (a qualified alias is not valid SQL). They are for source-controlled
column names, not user input. Each returns a trusted `PgSqlFragment`, so it
drops straight into a `{name}` placeholder — no `unsafe_sql(...)` wrapper — and
carries the literal `'{}'` JSON path safely:

| Helper | Output |
|---|---|
| `uuid_text("id")` | `id::text AS id` |
| `uuid_text("vaults.id")` | `vaults.id::text AS id` |
| `nullable_uuid_text("receipt_id")` | `CASE WHEN receipt_id IS NULL THEN NULL ELSE receipt_id::text END AS receipt_id` |
| `timestamptz_json("created_at")` | `to_json(created_at)#>>'{}' AS created_at` |
| `timestamptz_json("vaults.created_at")` | `to_json(vaults.created_at)#>>'{}' AS created_at` |
| `nullable_timestamptz_json("finished_at")` | `CASE WHEN finished_at IS NULL THEN NULL ELSE to_json(finished_at)#>>'{}' END AS finished_at` |
| `columns([uuid_text("id"), "payload"])` | `id::text AS id, payload` |
| `select_clause([...])` | `SELECT ...` |

`columns([...])` joins projection parts — fragments or source-controlled
strings — into one fragment for a `{projection}` placeholder, so a whole column
list composes without `unsafe_sql(...)`:

```harn,ignore
let q = sql(
  "SELECT {projection} FROM receipts WHERE tenant_id = {tenant_id}::uuid",
  {
    projection: columns([uuid_text("id"), timestamptz_json("created_at"), "payload"]),
    tenant_id: tenant_id,
  },
)
// q.sql == "SELECT id::text AS id, to_json(created_at)#>>'{}' AS created_at, payload
//           FROM receipts WHERE tenant_id = $1::uuid"
```

## Transactions and tenant settings

Use `pg_transaction` for changes that must commit or roll back together. The
transaction handle is only valid inside the callback.

```harn
pg_transaction(
  db,
  { tx ->
    pg_execute(tx, "insert into event_log(tenant_id, kind, payload) values ($1, $2, $3)", [
      tenant_id,
      "receipt.created",
      {receipt_id: receipt_id},
    ])

    pg_execute(tx, "insert into audit_records(tenant_id, action) values ($1, $2)", [
      tenant_id,
      "receipt.created",
    ])
  },
  {settings: {"app.current_tenant_id": tenant_id}},
)
```

`settings` are applied with `set_config(name, value, true)`, making them local
to the transaction. This is the intended boundary for Postgres RLS policies
that read settings such as `current_setting('app.current_tenant_id', true)`.

## Savepoints

Wrap intermediate steps inside a transaction so the outer commit can keep
the surviving writes while the rolled-back ones disappear:

```harn
let drop_inner = true

pg_transaction(db, { tx ->
  pg_execute(tx, "insert into entries (id, label) values ($1, $2)", [1, "outer"])
  pg_savepoint(tx, "before_inner")
  pg_execute(tx, "insert into entries (id, label) values ($1, $2)", [2, "inner"])
  if drop_inner {
    pg_rollback_to_savepoint(tx, "before_inner")
  }
  pg_release_savepoint(tx, "before_inner")
})
```

Savepoint names must match `/^[A-Za-z_][A-Za-z0-9_.]*$/` and may be up to 63
bytes (the Postgres identifier limit). The runtime double-quotes them
before emitting `SAVEPOINT "name"`.

## Migrations

`pg_migrate` applies every `.sql` file under a directory that has not yet
been recorded in the ledger. Files are sorted lexicographically and each
file runs inside its own transaction guarded by a process-wide advisory
lock (so concurrent callers serialize cleanly). `.down.sql` siblings are
ignored — keep down migrations alongside ups for tooling outside Harn but
let Harn only apply the ups.

```harn
let pool = pg_pool("env:DATABASE_URL", {max_connections: 1})
let result = pg_migrate(pool, {dir: "./migrations"})
log("applied " + to_string(len(result.applied)) + " of " + to_string(len(result.available)))
```

The result dict carries `applied`, `skipped`, `available`, `dry_run`,
`duration_ms`, and `table`. Pass `{dir, dry_run: true}` to plan a run
without touching the database — `applied` lists what *would* run. The
ledger lives in a configurable table (default `harn_migrations`) with
columns `name TEXT PRIMARY KEY`, `applied_at TIMESTAMPTZ DEFAULT NOW()`,
and `checksum BYTEA` (SHA-256 of the file at apply time).

For richer migration tooling — multi-statement `.down` files, baselines,
or branching — keep using SQLx CLI, Sqitch, or Flyway and call
`pg_migrate` only when your `.harn` pipeline is the authoritative
schema owner.

### SQLx-compatible ledger

Pass `ledger: "sqlx"` to apply an existing SQLx migration history into
SQLx's own `_sqlx_migrations` table, byte-for-byte compatibly:

```harn
let pool = pg_pool("env:DATABASE_URL", {max_connections: 1})
let result = pg_migrate(pool, {dir: "./migrations", ledger: "sqlx"})
```

In this mode `pg_migrate`:

- Always targets the `_sqlx_migrations` table (the default
  `ledger: "harn"` continues to use `harn_migrations`). Passing a `table`
  that disagrees with `_sqlx_migrations` is an error.
- Identifies each migration by the **integer version prefix** of its
  filename (`splitn(_)[0]`, e.g. `20260419170000_bootstrap.up.sql` →
  `20260419170000`) rather than the full name, sorts ascending by that
  numeric version, and applies only forward files (`*.up.sql` / `*.sql`,
  skipping `*.down.sql`). A non-integer prefix is a hard error.
- Computes a **SHA-384** checksum of the raw file (the same digest SQLx
  records), so a migration already applied by `sqlx migrate run` is
  recognized and skipped — running `pg_migrate(ledger: "sqlx")` against a
  SQLx-migrated database applies zero rows.
- Records rows exactly as SQLx does
  (`version, description, success=TRUE, checksum, execution_time`).
- Takes the **same per-database advisory lock** SQLx uses
  (`0x3d32ad9e * crc32(current_database())`), so a Harn migration and a
  concurrent `sqlx migrate run` serialize against each other instead of
  racing.
- Refuses to run when the ledger has a failed (`success = false`)
  migration (a "dirty" ledger), and errors if a file's checksum differs
  from the recorded one (drift detection), naming the offending version.
- Honors a leading `-- no-transaction` comment by running the migration
  outside a wrapping transaction, matching SQLx.

This is the path to retire a Rust `run_migrations()` that called
`sqlx_core::migrate::Migrator`: point `pg_migrate(ledger: "sqlx")` at the
same `migrations/` directory and the two are interchangeable.

## Typed rows from migrations

`pg_query` / `pg_query_one` return rows as plain dicts. To get
compile-time field checking, generate a Harn record type per table from
your migration files and annotate the result:

```sh
harn pg codegen --dir migrations --out src/db_types.harn
```

This replays every forward migration (`.sql`, excluding `.down.sql`) in
lexicographic order — the same discovery rule `pg_migrate` uses — and
emits one `type <Table>Row = {…}` per table whose columns mirror the
live schema. `CREATE TABLE`, `ALTER TABLE … ADD/DROP/ALTER/RENAME
COLUMN`, `RENAME TO`, and `DROP TABLE` are all replayed, so the
generated file always reflects the schema state on disk. No live
database is required — this is build-time codegen.

```harn,ignore
import { ReceiptsRow } from "./db_types.harn"

let receipt: ReceiptsRow = pg_query_one(pool, "select * from receipts where id = $1", [id])
log(receipt.kind)   // type-checked against the column set
```

SQL types map to the same Harn types the row decoder produces at
runtime: integer families → `int`, `real`/`double precision` → `float`,
`numeric`/temporal/`uuid`/text families → `string`, `json`/`jsonb` →
`any`, `bytea` → `bytes`, `hstore` → `dict<string, string?>`, ranges →
`{start, end, start_inclusive, end_inclusive}`, geometric types → structured
point/line/path dictionaries, and `T[]` → `list<T>`. Columns that are not `NOT
NULL` (and not a primary key) become optional (`field: T?`).

Pass `--check` (with `--out`) to verify the generated file is current
without writing it — wire it into CI so a migration that changes a
column fails the build until the types are regenerated:

```sh
harn pg codegen --dir migrations --out src/db_types.harn --check
```

Inferring types from arbitrary `SELECT` projections is out of scope —
that needs a live `PREPARE` round-trip. Codegen reflects the table
shape; annotate `select *` queries, or hand-write a narrower shape for
projections.

## Advisory locks

Advisory locks coordinate work across processes that share a Postgres
instance — typically "only one worker may run job X at a time" without
having to write the lock state to a table. Transaction-scoped locks
auto-release on commit/rollback, which matches almost every real use
case (the lock should live exactly as long as the work it guards).

```harn
pg_transaction(db, { tx ->
  pg_advisory_xact_lock(tx, "release-cut", {tenant_namespace: true})
  // exclusive section — released when this fn returns or throws
})

if (pg_with_advisory_lock(db, 0x4861726E, { tx ->
  // opens an internal txn, takes the lock, runs the body, commits.
  return pg_query_one(tx, "select count(*) as n from receipts", []).n > 0
})) {
  log("had receipts")
}

// Non-blocking probe:
pg_transaction(db, { tx ->
  if (pg_try_advisory_xact_lock(tx, 0x4861726E)) {
    // …
  }
})
```

Keys may be an `int`, a `string` (hashed to a `(class, instance)` pair),
or `{class: int, instance: int}`. Pass `{tenant_namespace: true}` to XOR
the key with a tenant-id-derived salt so two tenants colliding on the
same caller-supplied key resolve to *different* server-side keys.

## LISTEN/NOTIFY

`pg_listen` opens a [`PgListener`][sqlx-listener] (sqlx's auto-reconnect
async subscriber) and returns a handle. `pg_listener_recv(handle, ms?)`
blocks up to `ms` milliseconds; pass `nil` for non-blocking. `pg_notify`
serializes its payload as JSON (string payloads pass through unchanged)
and emits the corresponding `NOTIFY` command.

[sqlx-listener]: https://docs.rs/sqlx/latest/sqlx/postgres/struct.PgListener.html

```harn
let listener = pg_listen(db, ["receipts.updated", "captain.notice"])
while (true) {
  let n = pg_listener_recv(listener, 5000)
  if (n == nil) { continue }
  log(n.channel + " -> " + n.payload)
}
pg_listener_close(listener)

pg_notify(db, "receipts.updated", {receipt_id: "r1"})
```

Pass `{bridge_to_channel: true}` to `pg_listen` to republish every
received notification onto the in-process channel bus as
`pg:<channel-name>` — useful for composing with the trigger DSL.

## JSONB helpers

The `pg.jsonb.*` helpers execute Postgres' native JSONB functions/operators with
bound operands, which keeps quick JSON manipulation out of string-built SQL:

```harn
let ids = pg.jsonb.path(db, {items: [{id: 1}, {id: 2}]}, "$.items[*].id")
let merged = pg.jsonb.merge(db, {a: 1}, {b: 2})
let has_b = pg.jsonb.contains(db, merged, {b: 2})
```

Use ordinary `pg_query` for predicates over table columns; these helpers are for
JSON values already available to the Harn program.

## Pool observability

```harn
let stats = pg_pool_stats(db)
// → {size, idle, in_use, max_connections, statement_cache_capacity,
//    read_routing_policy, replicas, circuit_state, circuit_failures,
//    circuit_opened_at_ms}
```

`circuit_state` is `"disabled"` unless `circuit_breaker` was passed to
`pg_pool(...)`. When enabled, consecutive failure budgets are tracked
per pool; once `failure_threshold` is reached the circuit opens and
queries fast-fail with `pg: circuit open` until `reset_after_ms`
elapses, then a single half-open probe runs.

## Schema introspection

```harn
pg_introspect_tables(db, {schema: "public"})
// → [{schema, table, kind}, …] where kind is one of
//   table / partitioned_table / view / materialized_view / foreign_table.

pg_introspect_columns(db, "billing.invoices")
// → [{column, type, data_type, nullable, default}, …]

pg_introspect_indexes(db, "billing.invoices")
// → [{index, columns, unique, primary}, …]
```

Identifiers are validated against the standard PG identifier rules
(`[A-Za-z_][A-Za-z0-9_]*`, ≤ 63 bytes) and bound as parameters — no
string concatenation hits the wire.

## Read replicas

```harn
let db = pg_pool("env:DATABASE_URL", {
  max_connections: 10,
  replicas: ["env:DATABASE_REPLICA_URL", "env:DATABASE_REPLICA2_URL"],
  read_routing_policy: "round_robin_replica",
})
// Per-query opt-in uses the pool's read_routing_policy.
pg_query(db, "select * from receipts where id = $1", [id], {read_only: true})
// Or override the route for one query.
pg_query(db, "select count(*) from receipts", [], {route: "primary"})
// Writes always go to the primary.
pg_execute(db, "insert into receipts (id, payload) values ($1, $2)", [id, payload])
```

`replicas` accepts URL strings, `env:…`/`secret:…` references, or
`{url|env|secret}` dicts — the same shapes the primary URL accepts. The default
`read_routing_policy` is `replica_or_primary`, which round-robins across
replicas when present and falls back to primary when none are configured.
`replica` and `round_robin_replica` require at least one replica and fail fast
otherwise.

## Partition helpers

```harn
pg_partition_attach(db, "events", "events_2026_05",
                    {from: "2026-05-01", to: "2026-06-01"})       // range
pg_partition_attach(db, "events", "events_h0", {modulus: 4, remainder: 0})  // hash
pg_partition_detach(db, "events", "events_2026_03", {concurrently: true})
let pruned = pg_partition_prune(db, "events", "2026-01-01")
// Returns the list of `<schema>.<partition>` names that were dropped.
// Pass {dry_run: true} to compute the list without dropping.
```

Bounds may be `{from, to}` (range), `{in: [...]}` (list),
`{modulus, remainder}` (hash), or `{default: true}` (default
partition). Caller-supplied bounds are rendered as SQL literals — keep
them constant and trusted.

### Retention and maintenance

```harn
// Drop every partition whose data is entirely older than 90 days. This
// is the one-call form of `pg_partition_prune(db, parent, now - 90d)`.
let dropped = pg_partition_retain(db, "events", {keep_days: 90})
// `{keep_hours: N}` retains an hour-granular window instead.

// Pre-create the next 7 daily partitions (pg_partman's run_maintenance
// equivalent) so inserts never fall through to the DEFAULT partition.
let created = pg_partition_create_for_window(db, "events",
                {interval: "day", ahead: 7})
// `interval` is "day" or "hour"; `start` (an ISO date/timestamp)
// defaults to now. Child partitions are named `<table>_<YYYY_MM_DD[_HH]>`
// and only the missing ones are created. Returns the created names.
```

Both `pg_partition_retain` and `pg_partition_create_for_window` accept
`{dry_run: true}` to compute the result list without touching the
database. `pg_partition_prune` and `pg_partition_retain` descend
sub-partition trees recursively, so two-level layouts (range-by-day →
hash-by-tenant, or the inverse) prune correctly regardless of which
level carries the time column.

## Extended column decoding

Beyond the v1 primitive types, the row decoder also handles:

- `HSTORE` → `dict<string, string | nil>`
- `POINT` → `{x: float, y: float}`
- Other geometric types (`LINE`, `LSEG`, `BOX`, `PATH`, `POLYGON`,
  `CIRCLE`) fall back to PG's textual representation.

## Array column decoding

The row decoder handles common array types end-to-end: `BOOL[]`,
`INT2[]`, `INT4[]`, `INT8[]`, `FLOAT4[]`, `FLOAT8[]`, `TEXT[]`,
`VARCHAR[]`, `UUID[]`, `JSON[]`, `JSONB[]`. Other array element types
fall back to their textual representation.

## Mock fixtures

Tests can avoid a live Postgres server with `pg_mock_pool`.

```harn
let db = pg_mock_pool([
  {
    sql: "select id, payload from receipts where tenant_id = $1",
    params: ["tenant-123"],
    rows: [{id: "r1", payload: {ok: true}}],
  },
  {
    sql: "insert into audit_records(tenant_id, action) values ($1, $2)",
    params: ["tenant-123", "receipt.read"],
    rows_affected: 1,
  },
])

let rows = pg_query(db, "select id, payload from receipts where tenant_id = $1", ["tenant-123"])
assert_eq(rows[0].payload.ok, true)

let result = pg_execute(db, "insert into audit_records(tenant_id, action) values ($1, $2)", [
  "tenant-123",
  "receipt.read",
])
assert_eq(result.rows_affected, 1)
assert_eq(len(pg_mock_calls(db)), 2)
```
