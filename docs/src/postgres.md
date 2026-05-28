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
