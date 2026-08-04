# `harn usage` — LLM spend and usage analytics

`harn usage` turns the per-call cost and token data Harn **already records** in
the event log into spend/usage rollups by provider, model, or a
day/week/month time series — including prompt-cache efficiency.

Every LLM call the runtime makes appends a `provider_call_response` event to the
`agent.transcript.llm` topic in `<project>/.harn/events.sqlite`. That event
already carries `provider`, `model`, token counts, cache telemetry, and the
runtime-computed `cost_usd`. `harn usage` reads those events through the same
event-log reader `harn portal` uses, filters to `provider_call_response`
records, and aggregates them. It does **not** recompute pricing — `cost_usd` is
summed straight from the event.

## Quick start

```bash
# Default: by-provider table for the current project's event log, all-time.
harn usage

# A specific project.
harn usage --project ~/projects/my-service

# Roll up across every discoverable project event log.
harn usage --all
```

Example output:

```text
harn usage — 4329 calls across 1 source, $18.8753 by provider

provider               calls        cost      in_tok    out_tok   cache%  cache_save    ms/call
openrouter              2880    $16.5757    62298223    1096570    25.8%     $1.8904       3781
cerebras                 613     $2.2948     7622388     293140    41.0%     $0.0000        961
anthropic                  1     $0.0019          10        123     0.0%    $-0.0079       4710
llamacpp                 829     $0.0000    12640752     177160    40.6%     $0.0000      14967

total: 4329 calls, $18.8753, 82566555 in / 1567871 out tok, $1.8826 cache savings
```

## Options

| Flag | Description |
| --- | --- |
| `--project <path>` | Project root whose `.harn/events.sqlite` to read. Defaults to the current directory. |
| `--all` | Discover and aggregate across every `<project>/.harn/events.sqlite` found under the current directory and `~/projects`, skipping vendored `.cargo/registry` copies. |
| `--since <date>` | Only include calls at or after this date (inclusive). Accepts `YYYY-MM-DD` or an RFC3339 timestamp. |
| `--until <date>` | Only include calls strictly before this date. A bare `YYYY-MM-DD` is inclusive of the whole day. |
| `--group-by provider\|model\|day\|week\|month` | Roll-up dimension. Defaults to `provider`. |
| `--provider <p>` | Only include rows for this provider. |
| `--model <m>` | Only include rows for this model. |
| `--json` | Emit the stable [`JsonEnvelope`](./cli-json-contract.md) instead of the text table. |
| `--csv` | Emit CSV rows instead of the text table. |

`mock`-provider rows are excluded by default so fixture and offline-demo calls
never contaminate real spend.

## Metrics

Each group reports:

- `calls` — number of `provider_call_response` records
- `cost_usd` — sum of the runtime-computed per-call cost
- `input_tokens` / `output_tokens`
- `cache_read_tokens` / `cache_write_tokens`
- `cache_savings_usd` — sum of the runtime's per-call cache-savings estimate
- `cache_hit_ratio` — call-weighted mean of `cache_read / (input + cache_read)`
- `mean_response_ms`
- `cumulative_cost_usd` — running cost total, on the `day`/`week`/`month` series

## Time series

```bash
harn usage --group-by day --since 2026-06-01
```

Day/week/month group-bys are ordered chronologically and carry a
`cumulative_cost_usd` column so you can see spend accrue over the window.

## JSON envelope

`--json` returns the same versioned envelope shape every `harn … --json`
command uses (`schemaVersion`, `ok`, `data`, `error`, `warnings`). The `data`
payload is a `UsageReport` with `group_by`, `sources`, `calls`, `groups`, and
`totals`. Agents can negotiate compatibility via `harn --json-schemas`, which
lists `usage` and its schema version.

## Scope and limitations

- The event log is a **lower bound** on spend: it rotates, and it only records
  LLM calls made through Harn orchestration. Non-Harn spend is not captured.
- Reconciling against provider billing APIs / Langfuse (`--backfill`) is a
  planned follow-up. It is not implemented in v1 and errors if requested.

## Related

- [Harn portal](./portal.md) — the observability UI over the same event log.
- [CLI `--json` contract](./cli-json-contract.md).
