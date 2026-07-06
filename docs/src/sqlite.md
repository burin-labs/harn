# SQLite

`std/sqlite` exposes VM-native SQLite helpers for Harn scripts that need a
local, portable relational store without running a database server.

```harn
import "std/sqlite"

pipeline default() {
  const db = sqlite_open(".harn/events.sqlite", {
    create: true,
    busy_timeout_ms: 5000,
    journal_mode: "wal",
    foreign_keys: true,
  })

  const rows = sqlite_query(
    db,
    "select topic, id, payload from events where topic = ? order by id desc limit ?",
    ["agent_events", 50],
  )

  log(json_stringify(rows))
  sqlite_close(db)
}
```

## Functions

| Function | Returns | Notes |
|---|---|---|
| `sqlite_open(path, options?)` | `SqliteDb` | Open `:memory:` or a file-backed database. |
| `sqlite_query(handle, sql, params?)` | `list<dict>` | Run a parameterized query and return decoded rows. |
| `sqlite_query_one(handle, sql, params?)` | `dict` or `nil` | Return the first row, or `nil` when no rows match. |
| `sqlite_execute(handle, sql, params?)` | `SqliteExecuteResult` | Run one statement and return `{rows_affected}`. |
| `sqlite_transaction(db, fn(tx) -> any, options?)` | closure result | Begin a transaction, pass a scoped `SqliteTx`, commit on normal return, rollback when the closure throws. |
| `sqlite_savepoint(tx, name)` | `bool` | Create a savepoint inside an open transaction. |
| `sqlite_release_savepoint(tx, name)` | `bool` | Release a savepoint. |
| `sqlite_rollback_to_savepoint(tx, name)` | `bool` | Roll back to a savepoint while keeping the outer transaction open. |
| `sqlite_migrate(db, {dir, table?, dry_run?})` | `SqliteMigrateResult` | Apply pending `.sql` files from a directory. |
| `sqlite_close(db)` | `bool` | Close and unregister a database handle. |
| `sqlite_mock_db(fixtures)` | `SqliteMockDb` | Create an in-process fixture-backed database for tests. |
| `sqlite_mock_calls(mock)` | `list<dict>` | Inspect recorded mock calls. |

## Open Options

`sqlite_open(":memory:")` creates a private in-memory database. File paths use
the same filesystem path policy as other stdlib file operations.

Options:

| Option | Default | Notes |
|---|---:|---|
| `create` | `false` | Create the database file and parent directories. Without it, missing files fail. |
| `read_only` | `false` | Open with SQLite read-only flags. Write helpers and migrations reject read-only handles. |
| `busy_timeout_ms` | `5000` | Wait for transient local writer contention before returning `database is locked`. |
| `journal_mode` | unset | Optional `delete`, `truncate`, `persist`, `memory`, `wal`, or `off`; ignored for read-only opens. |
| `foreign_keys` | `true` | Enables SQLite foreign-key enforcement on the connection. |

Extension loading is not exposed by `std/sqlite`.

## Parameters and Decoding

Always pass dynamic values through the `params` list:

```harn
const row = sqlite_query_one(
  db,
  "select id, payload from events where topic = ? and id > ?",
  ["agent_events", 100],
)
```

Primitive Harn values bind as null, booleans (`0` or `1`), integers, reals,
text, blobs, or integer durations. Compound values bind as JSON text.

Rows decode into dictionaries keyed by SQLite column name. SQLite's dynamic type
affinity is preserved: nulls become `nil`, integer and real columns become Harn
numbers, text becomes `string`, and blobs become `bytes`. JSON text remains text
unless the query decodes it explicitly with SQLite JSON functions.

## Transactions and Savepoints

`sqlite_transaction` opens `BEGIN IMMEDIATE` by default. Pass
`{mode: "deferred"}` or `{mode: "exclusive"}` when that SQLite transaction mode
is required.

```harn
sqlite_transaction(db, { tx ->
  sqlite_execute(tx, "insert into notes(id, body) values (?, ?)", [1, "outer"])
  sqlite_savepoint(tx, "before_optional")
  sqlite_execute(tx, "insert into notes(id, body) values (?, ?)", [2, "optional"])
  sqlite_rollback_to_savepoint(tx, "before_optional")
  sqlite_release_savepoint(tx, "before_optional")
})
```

The outer database handle is blocked while a transaction is active; use the
transaction handle passed to the closure. Savepoint names must match
`/^[A-Za-z_][A-Za-z0-9_.]*$/`.

## Migrations

`sqlite_migrate` applies every `.sql` file under a directory that has not yet
been recorded in the ledger. Files are sorted lexicographically. `.down.sql`
siblings are ignored.

```harn
const result = sqlite_migrate(db, {
  dir: "./migrations/sqlite",
  table: "harn_sqlite_migrations",
})
```

The result dict carries `applied`, `skipped`, `available`, `dry_run`, and
`table`. Pass `{dry_run: true}` to list pending migrations without applying
them.

## Mocking

Mocks match exact SQL text after trimming plus exact params when `params` is
provided:

```harn
const db = sqlite_mock_db([
  {sql: "select id from events where topic = ?", params: ["agent_events"], rows: [{id: 1}]},
])

const rows = sqlite_query(db, "select id from events where topic = ?", ["agent_events"])
const calls = sqlite_mock_calls(db)
```

## SQLite vs Postgres

`std/sqlite` and `std/postgres` intentionally share only familiar call shapes:
open/connect, `query`, `query_one`, `execute`, transactions, savepoints,
migrations, and deterministic mocks.

They do not share a generic `std/sql` abstraction. SQLite has local file modes,
in-memory databases, WAL pragmas, dynamic type affinity, and `?` parameters.
Postgres has network pools, `$1` parameters, schemas, JSONB, server-side
settings, and richer type metadata. Keep dialect-specific SQL in the module
that actually runs it.
