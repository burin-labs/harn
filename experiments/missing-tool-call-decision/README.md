# Missing tool-call classifier measurement

## Development result, 2026-09-22

This is a curated fidelity measurement, not a deployment accuracy or coding-task
success claim. Twelve fixed assistant texts contain six immediate tool intents
and six final answers, questions, quotations, or explanations. English, Spanish,
and French each contribute four texts. Five repeats per text and three arms
produce 180 completing runs. Arm order rotates; row order is a deterministic hash
of repeat and row identity. No example was removed after observing its result.

`replay.harn` invokes the canonical agent loop and missing-call consumer. Primary
assistant turns and tool responses are replayed; only the classifier contacts a
provider. The expected label controls subsequent replay fixture turns and never
enters classifier state. The comparison therefore measures recovery decisions and
classifier overhead, not whether a live coding agent subsequently solves a task.

| Arm | Correct recovery and tool | False recoveries | Missed recoveries | Median classifier ms | Cost for 60 runs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Legacy structured, GPT-5.4 nano | 55/60 | 0 | 5 | 1,245 | $0.00999725 |
| Structured evaluator, GPT-5.4 nano | 57/60 | 3 | 0 | 1,318 | $0.01418100 |
| Native evaluator, Jev 1.13 | 60/60 | 0 | 0 | 203 | $0.00169680 |

All arms completed 60 replay runs with zero transport failures. Total measured
study cost was $0.02587505 against a predeclared $2 ceiling. Provider-reported
revisions were `gpt-5.4-nano-2026-03-17` and
`typesafe/jev-1.13-20260917`. The legacy checkpoint does not report an authoritative
served revision. Both structured arms reported zero cache-read tokens in all 60
calls; native cache telemetry was unavailable in all 60 calls.

Native and structured evaluator recovery decisions agreed on 57/60 paired
trials; their three-way action labels agreed on 47/60. Ambiguous answers safely
avoided recovery on negative examples, so correct recovery is different from raw
answer accuracy. The calibration report records native intent accuracy 52/60 and
structured intent accuracy 53/60 before consumer thresholding. It is a descriptive
report over repeated curated examples; its threshold recommendation is not used
to certify deployment or select a new threshold.

The structured evaluator falsely recovered on three Spanish permission-question
trials. In all five repeats its intent answer was incorrectly true at confidence
0.9 or 0.95; lower tool confidence prevented two recoveries. The legacy classifier
missed the same English immediate-edit example in all five repeats. These failures
remain part of the evidence and disprove behavioral equivalence of the initial
structured replacement.

Paired bootstrap intervals resample the 12 text identities, keeping each text's
five repeats together. Native minus legacy mean classifier latency was -1,021.9 ms
(95% interval -1,073.7 to -970.2); structured minus legacy was +94.1 ms (32.6 to
151.4). Recovery accuracy intervals include zero improvement for both candidates;
the small curated corpus cannot establish general reliability.

## Reproduction inputs and evidence

`assistant-texts.jsonl` is the fixed corpus. `replay.harn` accepts a classifier
configuration JSON path and one corpus-row JSON path. `calibration.harn` accepts
normalized labeled observations and calls the owning `std/eval/calibration`
primitive. `development-analysis.json` and `development-calibration.json` preserve
the complete aggregate results. Raw execution records and provider receipts are
retained in the task's OS-temporary artifact directory, not committed run storage.

The tested candidate contains signed source
`9c9b28cf88d77e204140e3f01da65b20cc0ee7f4`, binary SHA-256
`3d2f3e1e8db99926a031e5f4af0a6a18ec0ba7ca795d212ea9931ba87a002c80`.
Legacy source is `3f6ad0bd3aa72740c1a326580e22de6098ab933f`, binary SHA-256
`b5b450f2f7b6f44a16497f03f5ae126e0fc345293b5fb1c3cf93a3385f37f18b`.

Instrument pilots are retained separately: the initial replay used zero-based
iteration indexing and made no call; later controls caught a wire identifier used
instead of a catalog identifier and unsupported native effort/temperature knobs.
Paid observations from those pilots are not silently added to or substituted for
the fixed study. Their combined cost was $0.00081110, making total spend including
instrument pilots $0.02668615. The CLI summary omitted native classifier calls, so measurement
uses settled session cost and the classifier checkpoint or evaluation receipt's
physical attempts. Native aggregate call count was also zero despite a measured
receipt and settled cost; it is not treated as absence of a call.

## Prespecified conditional holdout

Before editing the rubric, `conditional-holdout.jsonl` was fixed at SHA-256
`a9e80f54442205e80ab3c3020be2356a106b0f7986b9f8414f99b617fa3f2a48`.
It contains 12 different texts: in each language, two permission or hypothetical
negatives and two approved or committed immediate positives. The comparison is
five repeats across four arms: legacy structured, original structured evaluator,
revised structured evaluator, and revised native evaluator. Models, thresholds,
replay procedure, and deterministic rotating order remain fixed. The total spend
ceiling is $1. All pilots and failures are retained. This holdout will not be used
for repeated rubric tuning or deployment certification.
