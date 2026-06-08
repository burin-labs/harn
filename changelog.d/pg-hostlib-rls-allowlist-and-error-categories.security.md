- **`std/postgres` transaction settings are now allowlisted, and raw Postgres
  error detail no longer leaks to `.harn` callers.** Two hardening fixes to the
  Postgres hostlib. (1) `pg_transaction(settings)` previously ran `set_config`
  for *any* GUC key, so `.harn` code could set a privileged backend GUC
  (`role`, `session_authorization`, `is_superuser`, `search_path`, …) to escape
  row-level security at the Postgres level. Settings are now restricted to the
  application's own `app.*` namespace (which RLS policies are written against)
  plus the benign `statement_timeout` / `lock_timeout` /
  `idle_in_transaction_session_timeout` GUCs; any other key is rejected with a
  clear error. (2) A failing query/execute previously surfaced the raw Postgres
  message — which embeds schema, relation, column, and constraint names — to the
  caller. The hostlib boundary now maps database errors to stable, schema-free
  categories (e.g. `unique_violation (SQLSTATE 23505)`) and keeps the full
  detail in server-side tracing only.
