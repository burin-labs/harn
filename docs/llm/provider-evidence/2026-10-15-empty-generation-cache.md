# 2026-10-15 Anthropic empty-generation cache measurement

Keep the existing retry policy for #7603. All measured cache-shape changes cost more,
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
The separate five-minute-TTL comparison used the same synthetic input with a
dated prefix, totaling 8,590 input tokens. Its warm and restored-prompt controls
both read 8,587 cached tokens. Changing only the last message from "Confirm
receipt." to "Confirm receipt again." caused a full cache write. After a measured
310,002 ms wait, the unchanged request read zero cached tokens and wrote 8,587,
confirming expiry through usage as well as elapsed time.

| Shape in TTL comparison | Catalog-priced call cost | Increment over warm call |
| --- | ---: | ---: |
| Warm cache read | $0.0026451 | $0 |
| No cache breakpoint | $0.0258300 | $0.0231849 |
| Request after expiry | $0.03227025 | $0.02962515 |
| Changed last message | $0.0322740 | $0.0296289 |

The expiry alternative also delayed the request by over five minutes. The
perturbation alternative changes the prompt. Neither has demonstrated recovery
benefit here, so neither earns a runtime policy change from these results.

The full measurement cost was **$1.3784337**, covering 422 physical provider
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
