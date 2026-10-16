# Empty-generation cache receipts

The [measurement report](../../llm/provider-evidence/2026-10-15-empty-generation-cache.md)
records the decision and its limits. `summary.json` contains the sample census,
runtime identities, measured costs, and SHA-256 hashes of the raw JSONL files.

`calibration.jsonl`, `parallel-control-only.jsonl`, and `calls.jsonl` came from
Harn 0.10.163. `v167/calls.jsonl` came from Harn 0.10.167. Each frequency cohort
contains exactly indices 0 through 199. The remaining rows are controls and
stay outside that denominator.

The [probe](../../../scripts/probes/2026_10_15_empty_generation_cache.harn)
retains the serial probe used for both cohorts, formatted after
execution. It uses synthetic text and records public response text plus usage;
no credentials or private input are included. Running it makes 204 paid calls.
