# Replay benchmark suite

`harn bench replay` reads `suite.json` by default and emits a stable JSON
artifact for Harn Cloud replay-determinism leaderboard ingestion.

The suite references canonical replay-oracle traces instead of duplicating
them. That keeps pass/fail replay semantics and benchmark scoring on the same
trace contract:

- `simple_tool_run`: deterministic read-only tool call.
- `composition_readonly`: Code Mode parent and child tool receipts.
- `permission_gated_edit`: HITL permission decision and file-effect receipt.
- `event_triggered_multi_step_workflow`: event-triggered worker handoff with
  protocol, transcript, and artifact material.

Run locally:

```sh
cargo run --bin harn -- bench replay --json --output replay-benchmark.json
```

External adapter fixtures live under `adapters/`. They document the
OpenCode-inspired JSONL shape accepted by:

```sh
harn bench replay \
  --adapter opencode-jsonl \
  --external-first benchmarks/replay/adapters/opencode/first.jsonl \
  --external-second benchmarks/replay/adapters/opencode/second.jsonl
```
