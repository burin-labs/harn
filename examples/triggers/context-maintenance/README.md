# Context maintenance hooks

Canonical lifecycle-hook package for non-blocking background context work.

- `context.refresh` is the deterministic hot lane for file edits and post-turn
  refresh.
- `context.crystallize` is the slower librarian/crystallization lane for idle,
  pre-compact, and session-end work.
- Hook handlers return `harn.context_maintenance.job_receipt.v1` receipts and
  leave queueing, persistence, and worker execution to the host.

## Verify

```sh
harn check lib.harn
harn run examples/context-maintenance-demo.harn
```
