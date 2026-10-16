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

`cache-variants.jsonl` and `cache-variant-no-cache.jsonl` came from Harn
0.10.167. The [variant probe](../../../scripts/probes/2026_10_15_cache_variants.harn)
makes five paid calls, including a 310-second expiry wait. Its archived guards
use explicit throws in place of the executed probe's assertions; provider inputs
are unchanged. The [uncached control](../../../scripts/probes/2026_10_15_cache_variant_no_cache.harn)
makes one paid call and was run during that wait. The elapsed-time row is
metadata, not a provider call. These six controls stay outside the frequency
denominator.
