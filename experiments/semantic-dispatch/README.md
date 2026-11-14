# Semantic dispatch smoke results

On November 14, 2026 UTC, 96 live requests exercised the fixed order-routing
definition: 12 synthetic cases, two repetitions, and four model routes.
Six cases were development inputs and six were holdout inputs. The policy was
fixed before the first live pass and wasn't tuned against either split.

| Route | Holdout decisions correct | Holdout abstentions | Holdout median | Cost for 24 requests |
| --- | --- | --- | --- | --- |
| Jev 1.14 | 12/12 | 0/12 | 228 ms | $0.000733488 |
| Decider 1.3, OpenRouter free route | 12/12 | 0/12 | 338 ms | $0, reported |
| Clef Flash | 5/12 | 7/12 | 347 ms | $0.000289968 |
| GPT-6 Luna structured baseline | 12/12 | 0/12 | 2,601 ms | $0.0035279 |

None of the native assessments selected a wrong handler after the dispatch
thresholds passed. Clef Flash's lower coverage shows that the same numerical
thresholds aren't interchangeable between models. The baseline produced labels
and self-reported confidence; it never invoked a handler.

The baseline used no dispatch threshold. Its zero abstentions measure label
production, not coverage under the native acceptance policy. Reference labels
were authored with the cases before inference; they weren't independently
reviewed by a second annotator.

The native arms run `orders().assess`. The separate HTTP smoke runs actual
lookup, review, investigation, and unmatched paths. Its investigation must
complete naturally and successfully invoke `delivery_record`; a selected label
alone doesn't pass that smoke.

[`http-smoke.jsonl`](./http-smoke.jsonl) records those four HTTP responses,
including the investigation's natural terminal state and successful tool call.

## Evidence and reproduction

[`manifest.json`](./manifest.json) names every arm and observation file.
[`results.json`](./results.json) contains the generated report, calibration
curves, abstention reasons, costs, and served identities. Each raw observation
records its definition and corpus digests, selected evidence, and retained
evaluation accounting. Harn records one client transport attempt for each of
the 96 requests. Upstream attempt counts weren't reported.

From the repository root, regenerate the report:

```sh
harn run examples/semantic-dispatch/analyze.harn -- experiments/semantic-dispatch/manifest.json
```

Record a new arm with an available provider credential:

```sh
harn run examples/semantic-dispatch/study.harn -- openrouter/typesafe/jev-1.14 native_decision 2
```

The runner prints one JSON observation per line. Labels stay outside the
model-visible state. Add the observation file to a new manifest before analysis.
The analyzer refuses missing cases, duplicate observations, changed labels,
changed definitions, and zero provider attempts. Unknown cost remains unknown.

OpenRouter served `typesafe/jev-1.14-20261014` and
`perplexity/pplx-decider-v1.3-20261112`. Clef Flash and the generative baseline
returned no immutable revision suffix. Their results aren't admission evidence
for a future alias revision.

## Limits

This is a smoke study with six unique holdout inputs, not a production sample.
Repeated trials don't create independent examples. Calibration curves and
derived recommendations describe these inputs; they don't qualify deployment
thresholds or establish a production error bound.

Jev, Clef Flash, and the baseline ran with at most three independent requests
in flight across routes. Decider ran after its registration was added.
Latency measures the evaluator call and local assessment, excluding HTTP
handling and the investigation agent. No production throughput claim follows.

The free Decider price is the price actually reported for this route and date.
It isn't a service-level guarantee or a forecast. The Vercel Decider route
returned HTTP 400 under Harn's required privacy filters. That route remains
outside the catalog. OpenRouter receipts report that native decision
privacy controls weren't available; all inputs here are synthetic.
