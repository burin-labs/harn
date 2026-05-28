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
| `pg_migrate(pool, {dir, table?, dry_run?})` | `PgMigrateResult` | Apply `.sql` files from a directory; track the applied set in `harn_migrations` (override via `table`). |
| `pg_close(pool)` | `bool` | Close and unregister a pool handle. |
| `pg_mock_pool(fixtures)` | `PgMockPool` | Create an in-process fixture-backed pool for tests. |
| `pg_mock_calls(mock)` | `list<dict>` | Inspect SQL, params, and execute/query mode recorded by a mock pool. |

`source` may be a raw Postgres URL, `env:VARIABLE_NAME`, `secret:namespace/name`,
or a dict with one of `url`, `env`, or `secret`. `secret:` references use the
active Harn connector secret context, so they are available while executing a
Harn-backed connector export.

Pool options include `max_connections`, `min_connections`,
`acquire_timeout_ms`, `timeout_ms`, `idle_timeout_ms`, `max_lifetime_ms`,
`ssl_mode` or `tls_mode`, `application_name`, and
`statement_cache_capacity`. Prepared statement caching is driver-managed by
SQLx; tune it with `statement_cache_capacity` when needed.

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
`date`, `time`, `timestamp`, and `timestamptz`. Unknown types are decoded as
text when the Postgres driver can expose them that way.

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

## Pool observability

```harn
let stats = pg_pool_stats(db)
// → {size, idle, in_use, max_connections, statement_cache_capacity,
//    replicas, circuit_state, circuit_failures, circuit_opened_at_ms}
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
})
// Per-query opt-in routes through the round-robin replica cursor.
pg_query(db, "select * from receipts where id = $1", [id], {read_only: true})
// Writes always go to the primary.
pg_execute(db, "insert into receipts (id, payload) values ($1, $2)", [id, payload])
```

`replicas` accepts URL strings, `env:…`/`secret:…` references, or
`{url|env|secret}` dicts — the same shapes the primary URL accepts. The
round-robin cursor is shared across the pool; if replicas is empty the
read-only flag is a no-op.

## Partition helpers

```harn
pg_partition_attach(db, "events", "events_2026_05",
                    {from: "2026-05-01", to: "2026-06-01"})
pg_partition_detach(db, "events", "events_2026_03", {concurrently: true})
let pruned = pg_partition_prune(db, "events", "2026-01-01")
// Returns the list of `<schema>.<partition>` names that were dropped.
// Pass {dry_run: true} to compute the list without dropping.
```

Bounds may be `{from, to}` (range), `{in: [...]}` (list), or
`{default: true}` (default partition). Caller-supplied bounds are
rendered as SQL literals — keep them constant and trusted.

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
