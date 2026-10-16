# 2026-10-15 Anthropic empty-generation cache measurement

Keep the existing retry policy for #7603. The measured cache bypass costs more,
and neither live cohort produced an empty generation with which to measure a
recovery benefit. This decision does not establish that the historical failure
was fixed upstream or that cache shape caused it.

The [summary](../../evidence/7603/summary.json) identifies both runtime revisions
and hashes the retained receipts. The [probe](../../../scripts/probes/2026_10_15_empty_generation_cache.harn)
uses the canonical `harness.llm.call` path with a synthetic acknowledgement
prompt, an 8,577-token input, no tools, and a 32-token output limit.

| Runtime | Selected | Warm-cache eligible | Streaming | Non-streaming | Empty generations | Empty retries | Failed | Pending |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0.10.163 | 200 | 200 | 100 | 100 | 0 | 0 | 0 | 0 |
| 0.10.167 | 200 | 200 | 100 | 100 | 0 | 0 | 0 | 0 |

Every sample returned visible text, reported one physical provider call, and
read 8,574 cache tokens. The near-zero fresh-input condition was therefore
present. Four uncached controls reported zero cache reads and zero cache writes;
the initial cold control wrote 8,574 tokens. This distinguishes a measured miss
from a missing cache instrument.

| Shape | Catalog-priced call cost | Increment over warm call |
| --- | ---: | ---: |
| Warm cache read | $0.0026412 | $0 |
| No cache breakpoint | $0.025791 | $0.0231498 |
| Initial cold cache write | $0.0322215 | $0.0295803 |

Dropping the breakpoint costs about 2.315 cents extra for this input on
`anthropic:claude-sonnet-4-6`. <!-- harn-doc-model-ref: allow-stale -->
Waiting for expiry adds provider-specific delay
and can incur a new cache write; no expiry delay was measured. Request
perturbation changes the prompt and has no demonstrated recovery advantage
here. Neither alternative earns a runtime policy change from these results.

The full measurement cost was **$1.250499**, covering 416 physical provider
calls, including calibration and controls. All calls have priced usage; zero
calls have unknown usage. These are runtime catalog prices, not a reconciled
provider invoice. An earlier parallel probe executed only its four controls
before failing locally; those calls are retained and charged, but excluded
from the frequency denominator.

The original incident's model and prompts were not recorded in the issue.
These repeated requests share one warm cache and are not independent
production samples. The observed zero cannot establish a production rate or
disprove the original four incidents. If a fresh captured empty generation
recurs, compare unchanged and cache-bypassed retries against that exact request
before enabling a more expensive default.
