# Prompt-cache reports

`std/llm/cache_conformance` exposes the runtime cache classifier to scripts.
It reads provider capabilities and saved usage; it makes no provider requests.

## Interface

`report(llm: HarnessLlm, provider: string, model: string, runs: list<unknown>) -> CacheReport`

Each run is either a captured usage object or a record containing `usage`,
optional `request` identity hashes, and optional `elapsed_ms`. Preserve the
captured fields, including omissions. Raw provider usage and normalized Harn
usage use the same parser as the cache probe CLI.

The result contains the schema version, resolved support, normalized runs,
per-classification counts, aggregate verdict, and `dogfood_failure`. Input
tokens include cache reads and writes; `fresh_input_tokens` records the
uncached portion. Each run retains its raw usage and missing-field evidence.

Missing measurements produce `usage_unreported`; contradictory counters
produce `provider_field_inconsistent`. Neither is a measured cache miss.
An empty supported-route report produces `insufficient_runs`. Inspect the run
count alongside the verdict. A single warm run proves an observed cache read,
not repeat-run reliability.

```harn
import { report } from "std/llm/cache_conformance"

fn main(harness: Harness) {
  const result = report(harness.llm, "anthropic", "claude-sonnet-4-6", [
    {input_tokens: 40, cache_read_input_tokens: 5000, output_tokens: 8},
  ])
  assert_eq(result.runs[0].usage.input_tokens, 5040)
  assert_eq(result.runs[0].classification, "cache_effective")
}
```
