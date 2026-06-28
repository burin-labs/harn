# Replay benchmarks

`harn bench replay` scores replay determinism fixtures and emits a
machine-readable artifact for CI and cloud leaderboard ingestion.
It uses the same `harn.orchestration.replay_trace.v1` trace contract as
`harn orchestrator replay-oracle`; the benchmark layer adds comparable
metrics and stable receipt hashes.

## Run the canonical suite

```bash
harn bench replay --json --output replay-benchmark.json
```

By default the command reads `benchmarks/replay/suite.json`. That suite
references three canonical replay-oracle fixtures:

- simple tool run
- permission-gated edit
- event-triggered multi-step workflow

Run one file, directory, or suite manifest explicitly:

```bash
harn bench replay conformance/replay-oracle/fixtures/approval_tool_call.valid.json --json
harn bench replay conformance/replay-oracle/fixtures --filter worker --json
```

The command exits non-zero when any fixture fails its replay expectation,
so CI can gate on the process status while still uploading
`replay-benchmark.json` for inspection.

## Output contract

Reports use schema `harn.replay_benchmark.report.v1`. The public JSON
schema lives at `spec/schemas/replay-benchmark.v1.schema.json`.

Top-level fields:

- `cloud_ingest`: Cloud routing metadata. `kind` is
  `harn_cloud.replay_determinism.leaderboard.v1`.
- `suite`: suite name, source paths, and fixture count.
- `summary`: pass/fail counts, average fidelity, permission-preservation
  score, drift counts, and observed interaction totals.
- `fixtures`: one result per trace, including category metrics, first
  divergence, and a stable receipt hash.

The receipt hashes are derived from canonicalized replay trace material
after fixture allowlists have been applied. A cloud platform can ingest the
benchmark report without re-reading raw EventLog, transcript, or tool
payloads.

## Metrics

`determinism_score` is `1.0` when canonical replay output matches and
`0.0` when a meaningful divergence remains.

`replay_fidelity_score` is the fraction of populated trace sections that
match after allowlists. The compared sections are EventLog entries,
trigger firings, LLM interactions, protocol interactions, approvals,
effect receipts, persona runtime state, transcript deltas, final
artifacts, and policy decisions.

`permission_decision_preservation_score` compares approval interactions
and policy decisions. Fixtures with no permission surface score `1.0`
because there was no permission boundary to preserve.

`tool_call_drift_count` counts drift across LLM interactions, protocol
interactions, and effect receipts. `transcript_drift_count` counts drift
inside agent transcript deltas.

`debugging_time_to_root_cause_proxy` is not a time measurement. It is a
stable triage proxy based on the first divergent JSON path and the
number of drifted trace sections.

`runtime_cost` is an observed fixture-cost summary: interaction counts,
LLM token totals, and optional `cost_usd` values when fixtures contain
them. It does not benchmark wall-clock runtime.

## External trace adapter

The first adapter is `opencode-jsonl`, a documented OpenCode-inspired
JSONL shape for session events. It accepts line-delimited objects with
`type` values such as `message`, `tool_call`, `permission`, and `llm`,
then maps them into Harn replay trace buckets.

```bash
harn bench replay \
  --adapter opencode-jsonl \
  --external-first benchmarks/replay/adapters/opencode/first.jsonl \
  --external-second benchmarks/replay/adapters/opencode/second.jsonl \
  --external-name opencode-permission-run \
  --json
```

Adapter input is intentionally small and explicit. It is a bridge format
for benchmarking public/simple external traces, not a claim that every
OpenCode session log shape is natively supported.

## CI template

A copy-paste GitHub Actions template is checked in at
`docs/fixtures/github-actions/harn-replay-benchmark.yml`. It installs
the Harn CLI, runs `harn bench replay`, and uploads
`replay-benchmark.json` as a build artifact.
