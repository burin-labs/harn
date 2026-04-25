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

  println(json_stringify(rows))
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
| `pg_execute(handle, sql, params?)` | `{rows_affected: int}` | Run a statement that does not need returned rows. |
| `pg_transaction(pool, fn(tx) -> any, options?)` | closure result | Begin a transaction, pass a scoped `PgTx` handle to the closure, commit on normal return, rollback when the closure throws. |
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

## Parameters And Decoding

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

## Transactions And Tenant Settings

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

## Migrations

Schema migrations remain host-owned. Run migrations with your deployment
system, SQLx CLI, Sqitch, Flyway, or another migration runner before Harn
pipelines depend on the schema. For lightweight smoke checks, `pg_execute` can
run explicit DDL, but Harn does not maintain a migration ledger.

## Mock Fixtures

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
