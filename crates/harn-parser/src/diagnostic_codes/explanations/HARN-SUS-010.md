# HARN-SUS-010 — closed suspended worker cannot be resumed

The target worker was closed or cancelled after being suspended. Closing a
worker removes the suspension envelope and rejects future resume attempts
against that live worker handle.

Treat the worker as terminal. Spawn a new worker, or restore an explicit
snapshot path only when you intentionally want to inspect persisted state.
