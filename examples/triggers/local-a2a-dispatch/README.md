# Local / A2A Dispatch Demo

This example keeps the handler logic stable while moving dispatch across the
trust boundary.

- `harn.local.toml` runs `handlers::triage_issue` in process.
- `harn.remote.toml` changes only the trigger handler target to
  `a2a://127.0.0.1:8787/triage`.
- `remote-handler.harn` is the receiving A2A wrapper used by the demo script;
  it imports the same triage helper from `lib.harn`.

## Verify

```sh
harn check lib.harn
scripts/demo_local_a2a_dispatch.sh
```
