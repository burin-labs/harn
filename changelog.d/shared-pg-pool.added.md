- **Server embedders can install a process-lifetime shared Postgres pool
  registry.** `harn_vm::install_shared_pool_registry()` (re-exported as
  `harn_serve::install_shared_pool_registry()` under the `vm-postgres` feature)
  opts a long-lived server into reusing one `sqlx` connection pool per distinct
  connection identity across requests and worker threads — instead of opening a
  fresh pool on every `harn serve` dispatch (each request builds a new `Vm` on a
  thread the previous request's thread-local pool registry may not live on).
  Pools are keyed on the **resolved** connection identity (host/port/database/
  credentials, SSL mode, application name, replica set, and every pool-shaping
  option), not on a caller-supplied alias, so two callers never share a pool
  across different credentials, databases, or pool shapes. Safe across tenants
  because harn scopes RLS per-transaction (`set_config('app.current_tenant_id',
  …, true)`), never per-pool/per-connection. Strictly opt-in: the CLI one-shot
  path never installs the registry and behaves exactly as before.
