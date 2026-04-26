# Ship Captain Persona

Ship Captain is the Phase 0 Harn Flow persona. It watches stored Flow atoms,
groups them into intent summaries, derives a candidate slice, validates the
repo's invariant discovery surface, and emits an approval-gated mock PR receipt.

The v0 workflow is intentionally shadow-mode. It writes local receipts through
`harn flow ship watch` and does not open a remote GitHub PR.

Validate locally:

```bash
harn persona --manifest personas/ship_captain/harn.toml inspect ship_captain --json
harn check personas/ship_captain/manifest.harn
harn flow ship watch --store .harn/flow.sqlite --mock-pr-out .harn/flow/mock-pr.json --json
```
