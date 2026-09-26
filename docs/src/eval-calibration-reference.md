# `std/eval/calibration`

Reference for the calibration report primitive and `harn eval calibrate`.

For what a confidence number does and does not mean, read
[What a confidence score means](./concepts/confidence.md).

## `calibration_report(rows, options?) -> CalibrationReport`

Measures a classifier's stated confidence against known labels. The function is
the validation boundary: it normalizes every raw row itself and returns a typed
refusal for any corpus it cannot measure.

### Input rows

One row per corpus item. Only the first five fields are required.

| Field | Type | Meaning |
| --- | --- | --- |
| `question_id` | `string` | Which question this row answers. |
| `expected` | `string` | The correct label. |
| `predicted` | `string` | The label the classifier chose. Empty when it abstained. |
| `confidence` | `float` | The classifier's stated confidence, in `[0, 1]`. |
| `abstained` | `bool` | Whether the classifier declined to answer. |
| `backend` | `string?` | Which backend produced the answer. Defaults to `default`. |
| `cost` | `float?` | What the answer cost. |
| `latency_ms` | `float?` | How long the answer took. |

A boolean question uses the labels `"true"` and `"false"`. A score question
supplies its ordered legend through `options.level_order`, which also turns on
`mean_absolute_level_error`.

### Options

| Field | Default | Meaning |
| --- | --- | --- |
| `thresholds` | `[0.5, 0.7, 0.9]` | Finite acceptance probabilities in `[0, 1]`. |
| `target_error` | `0.05` | Finite target error strictly between `0` and `1`. |
| `seed` | `1` | Seed for the deterministic calibration/holdout split. |
| `model_revision` | `""` | Recorded in the report contract. |
| `served_model_id` | `""` | Recorded in the report contract. |
| `level_order` | `[]` | Ordered level labels for a score question. |

### Output

The result is a closed union. A report:

```harn
import "std/eval/calibration"

fn answer(expected: string, predicted: string, confidence: float) {
  return {
    question_id: "tool-safety",
    expected: expected,
    predicted: predicted,
    confidence: confidence,
    abstained: false,
  }
}

pipeline measure(harness: Harness, task: unknown) {
  const report = calibration_report(
    [
      answer("true", "true", 0.95),
      answer("true", "false", 0.55),
      answer("false", "false", 0.92),
    ],
    {thresholds: [0.9], served_model_id: "example/model@1"},
  )
  if report.kind == "refused" {
    harness.stdio.eprintln("no report: ${report.reason}")
    return
  }
  const error = report.groups[0].expected_calibration_error
  harness.stdio.println("calibration error ${to_string(error)}")
}
```

Out-of-range probability options return `invalid_options`. Typed option fields
reject nonfinite numbers at call admission; nonfinite row confidence returns
`invalid_confidence`. None produces a report or threshold.

Or a refusal, which is what an empty corpus, a one-row corpus, a missing label,
a confidence outside `[0, 1]`, or a label outside the supplied legend produces:

```harn
import "std/eval/calibration"

pipeline refuse(harness: Harness, task: unknown) {
  const report = calibration_report([])
  harness.stdio.println("${report.kind} ${report.reason}")
}
```

### Per-group numbers

Every group covers one `(question_id, backend)` pair and carries the row count
behind each number.

- `bins`: ten reliability bins over the rows the classifier answered. Each bin
  reports `rows`, `mean_confidence`, and `accuracy`.
- `expected_calibration_error`: the bin-weighted mean gap between
  `mean_confidence` and `accuracy`.
- `accuracy`, `correct_rows`, `scored_rows`, `abstained_rows`, `rows`.
- `thresholds`: one scored row per candidate threshold.
- `cost_usd` and `latency_ms`: `p50`, `p90`, and `max` over the rows that
  carried the field, with that row count.
- `recommendation`: the derived abstention threshold, or a typed refusal.
- `mean_absolute_level_error`: for score questions, the mean distance in legend
  steps between the predicted and expected level. `nil` without a legend.

### Threshold rows

A row is **accepted** at a threshold when the classifier did not abstain and its
confidence is at or above the threshold. Otherwise it is **withheld**.

| Field | Denominator | Meaning |
| --- | --- | --- |
| `coverage` | `rows` | Share of rows accepted. |
| `abstention_rate` | `rows` | Share of rows withheld. The complement of coverage. |
| `false_accept_rate` | `accepted` | Accepted answers that were wrong. |
| `false_reject_rate` | `withheld` | Withheld answers that were right. |
| `accepted_accuracy` | `accepted` | The complement of the false-accept rate. |

The two error rates are never averaged into one number. A gate that withholds
everything has a perfect false-accept rate and is useless. A withheld row the
classifier abstained on is not a false reject: it carried no prediction that
could have been right.

### Threshold recommendation

Split conformal prediction ([arXiv 2405.01563](https://arxiv.org/abs/2405.01563)).
The rows the classifier answered are split by a seeded deterministic shuffle
into a calibration half and a holdout half. The nonconformity score of a row is
`1 - confidence-of-correct`: `1 - confidence` when the prediction matched the
label, and `1.0` when it did not.

With `n` calibration rows and target error `e`, the threshold is `1 - q`, where
`q` is the `ceil((n + 1) * (1 - e))`-th smallest score.

The guarantee is **marginal coverage on exchangeable data**: for a new row from
the same distribution, the probability that it is both accepted and wrong is at
most `e`. It is an average over rows. It is not a statement about any one row
and not conditional on the confidence value.

`holdout_error` and `holdout_coverage` are measured on the half the threshold
was not fitted on. Fitting and measuring on the same rows reports how the
threshold does on data it already saw, which is optimistic by construction.

No threshold is recommended when:

- `reason: "split_too_small"` — fewer than two answered rows on either side.
- `reason: "target_unreachable"` — the calibration split is wrong too often, so
  `q` reaches `1.0` and no threshold can certify the target. A classifier that
  is confidently wrong lands here. It never yields a threshold of `0.0`.

## The report contract

Every report carries `contract: "harn.calibration_report.v1"`.

A consumer pins four fields in its policy: `corpus_digest`, `model_revision`,
`served_model_id`, and `report_digest`.

- `corpus_digest` is a SHA-256 over the normalized rows in canonical order. Two
  callers presenting the same corpus in a different order pin the same digest.
- `report_digest` is a SHA-256 over the canonical JSON of the report body.
  Harn serializes dictionary keys in sorted order, so the digest does not depend
  on the order the report was assembled in.

**Invalidation rule.** Re-run the report. If `report_digest` differs from the
pinned value, the pin is invalid and the policy that depends on it must not run
until it is re-derived. A provider alias that silently re-points to a new served
model changes `served_model_id`, which changes `report_digest`, so the alias
change cannot pass unnoticed. A Harn version that changes the report shape also
changes `report_digest`, intentionally: the numbers are no longer comparable.

## `harn eval calibrate`

```sh
harn eval calibrate --corpus corpus.jsonl --answers answers.jsonl \
  --thresholds 0.5,0.7,0.9 --target-error 0.05
```

Both files are JSONL, one row per line. The eval CLI already writes its per-row
output that way, so one format covers both sides and the command needs no second
parser.

Corpus row: `{id?, question_id, expected, input?}`.

Answer row: `{id?, question_id, predicted, confidence, abstained?, backend?,
cost?, latency_ms?}`.

The two files are joined on `id` when present and on line position when not. An
answer with no corpus row, or a corpus row with no answer, fails the command and
is named in the output. A silent inner join would let half a corpus produce a
confident report about the other half.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--corpus` | required | Labeled corpus JSONL. |
| `--answers` | required | Answers JSONL. |
| `--thresholds` | `0.5,0.7,0.9` | Candidate acceptance thresholds. |
| `--target-error` | `0.05` | Target error rate for the conformal threshold. |
| `--model-revision` | unset | Recorded in the report contract. |
| `--served-model-id` | unset | Recorded in the report contract. |
| `--json` | off | Print the report JSON instead of the rendering. |

The default rendering is plain language:

```text
calibration report harn.calibration_report.v1 over 40 rows, corpus 10079a7ad82d, report b5f567b8fb81
tool-safety question (default): 40 rows, 25.0 percent error over the 40 answered, 0.0 percent calibration error, 0 abstained
  at the 0.9 threshold: 5.0 percent of the 20 accepted answers were wrong, 50.0 percent withheld, 11 right answer(s) thrown away
  recommended threshold 0.55 for a 30.0 percent target error, fitted on 20 rows and measured at 30.0 percent error over 20 held-out rows
  latency over 40 rows: p50 139.5 ms, p90 155.1 ms, max 159.0 ms
```

The command exits `1` when the report is a refusal and `2` when the inputs
cannot be read or joined.
