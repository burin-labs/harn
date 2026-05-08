# Replay oracle fixtures

`harn orchestrator replay-oracle` validates canonical replay determinism
fixtures for full orchestration traces. Each fixture stores two observed runs
of the same logical trace and compares canonicalized streams for meaningful
drift.

The trace schema is `harn.orchestration.replay_trace.v1`. A fixture contains:

- `event_log_entries`
- `trigger_firings`
- `llm_interactions`
- `protocol_interactions`
- `approval_interactions`
- `effect_receipts`
- `agent_transcript_deltas`
- `final_artifacts`
- `policy_decisions`

Nondeterministic fields must be named explicitly in `allowlist` using
JSON-pointer-like paths. `*` matches every array element or object value. Common
allowlisted fields are EventLog offsets, timestamps, generated request ids,
latency, and model token accounting.

Run:

```sh
cargo run --bin harn -- orchestrator replay-oracle
```

or:

```sh
make replay-oracle
```

To prove drift fails loudly, run the mutation fixture directly:

```sh
cargo run --bin harn -- orchestrator replay-oracle \
  conformance/replay-oracle/mutations/meaningful_drift.invalid.json
```

The command exits non-zero and prints the first divergent canonical path.
