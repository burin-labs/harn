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
| Legacy structured, GPT-5.4 nano | 55/60 | 0 | 5 | 1,244.5 | $0.00999725 |
| Structured evaluator, GPT-5.4 nano | 57/60 | 3 | 0 | 1,315 | $0.01418100 |
| Native evaluator, Jev 1.13 | 60/60 | 0 | 0 | 202.5 | $0.00169680 |

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

The completed holdout is retained in `holdout-analysis.json` and
`holdout-calibration.json`. All 240 runs completed with zero transport failures,
costing $0.04065119. The revised candidate was source
`d6564e6e0926525225999191bf254301750f4f5e`, binary SHA-256
`316a1582024cdd354c171f4f392a5b50d63e969e41a9d86b0c339b65c0ec7d83`.

| Arm | Correct recovery and tool | False recoveries | Missed recoveries | Median classifier ms | Cost for 60 runs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Legacy structured | 50/60 | 0 | 10 | 1,288.5 | $0.01015250 |
| Original structured evaluator | 56/60 | 4 | 0 | 1,310.5 | $0.01392500 |
| Revised structured evaluator | 45/60 | 0 | 15 | 1,388.5 | $0.01469650 |
| Revised native evaluator | 60/60 | 0 | 0 | 203 | $0.00187719 |

The revised rubric removed false recoveries but regressed structured recall.
Its paired accuracy difference from the original structured evaluator was
-18.3 percentage points (row-cluster 95% interval -50.0 to +11.7). It is not
evidence of acceptable structured fallback equivalence. Both completed corpora
are now development evidence. Combined spend, including retained instrument
pilots, is $0.06733734 for 424 paid calls.

## Structured instruction placement ablation

The next candidate keeps the typed question rubric and answer schema fixed,
projects their shared descriptions into the system instruction, and stamps
`harn.evaluator.structured.v4`. State remains in the separate user message.
Native instructions and answer semantics are unchanged. This tests instruction
placement, not a new confidence threshold. The shared request admission must
count the added system text as well as the schema representation.

This is motivated by the model-dependent schema-versus-prompt findings in
[Lin, 2026](https://arxiv.org/abs/2608.08254), not a claim that placement alone
solves classification. A fresh corpus must be fixed before measuring this
candidate; neither completed corpus can become its qualification set.

`placement-final.jsonl` is fixed before any v4 call: twelve untouched texts,
six positives and six negatives, four per language. Five repeats compare legacy
GPT-5.4 nano, revised-rubric structured v3, the same rubric with structured v4,
and native Jev 1.13. The primary placement comparison is v4 minus v3, with
row-cluster paired intervals. Thresholds and tools remain unchanged. The 240-call
study has a $1 total ceiling, fixed rotating arm order and deterministic row
order. Instrument failures abort; behavioral mistakes remain observations. It
measures curated fidelity and overhead only, never deployment qualification.

### Placement result

All 240 calls completed with no transport or accounting failures, costing
$0.04536863. The API slot was released after completion. Candidate source is
`3f38fa8c4f59a6d4d47d71bb08c22fc6e4a965e6`; the immutable executable is
`/tmp/harn-8543-v4-3f38fa8c4/harn`, SHA-256
`dbac8b1fd5d65def4fe33164fcda92fee3fb12e6648ec2f9cee8c5105816cd83`.
The v3 control uses the revised rubric at `d6564e6e0926525225999191bf254301750f4f5e`.
Their input, question-set, and policy digests match; instruction version differs.
Both report the same GPT-5.4 nano snapshot. Legacy served revision remains unknown.

| Arm | Correct recovery and tool | False recoveries | Missed recoveries | Median classifier ms | Cost for 60 runs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Legacy structured | 55/60 | 0 | 5 | 1,210.5 | $0.01021750 |
| Revised rubric, structured v3 | 47/60 | 0 | 13 | 1,370.5 | $0.01467525 |
| Same rubric, structured v4 | 42/60 | 0 | 18 | 1,390 | $0.01859050 |
| Native evaluator | 60/60 | 0 | 0 | 203.5 | $0.00188538 |

The v4 minus v3 recovery difference is -8.3 percentage points (paired row-cluster
95% interval -23.3 to +3.3). Every v4 miss has a wrong false intent label; none
has correct labels rejected only by the confidence floor. Tool labels remain
correct. All 18 wrong intent labels have low confidence and become ambiguous.
The v3 control also has wrong false intent labels on all 13 misses, including
confident false answers. Lowering the floor would not correct those labels.

The production v4 projection is reverted. Schema descriptions were already
present, so instruction duplication was an empirical treatment, not a repair
for missing question semantics. It increased cost without a demonstrated quality
benefit. Production retains the matching v3 instruction identity. The exact v4
source remains in the signed commit above; `instruction-budget-v4.harn` and its
expected output preserve the canonical admission proof for that executable.
It refuses the expanded request before dispatch and admits a short control; the
v3 executable fails the expanded-request assertion. That accounting proof does
not establish classification quality.

`placement-analysis.json`, `placement-calibration.json`, and
`placement-miss-analysis.json` preserve the results. This final corpus remains
evaluation evidence and will not be used to tune thresholds or rubrics.
Native's 60/60 consists of twelve unique texts with five repeats, not a
deployment reliability estimate. Total spend across all three studies and
retained instrument pilots is $0.11270597 for 664 calls.

### Agreement instrument correction

All three analysis reports now count tool agreement only when both backends
actually recover. Earlier output vacuously counted non-recovery pairs as tool
matches. Eligible and matching counts are respectively 30/30 in development,
15/15 in the conditional holdout, and 12/12 in the placement study. Recovery
and action agreement counts are unchanged. `agreement-control.mjs` exercises
the same analyzer with a nonempty zero-eligible case, a measured match, and a
measured mismatch. None is a model-quality trial.
