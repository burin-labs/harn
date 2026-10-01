# Selective risk report reference

`std/eval/selective_risk::selective_risk_report(rows, options)` selects the
highest observed coverage whose conditional accepted-answer error has a
simultaneous upper bound below `target_error`.

This is a separate contract from the marginal guarantee in
[`calibration_report`](eval-calibration-reference.md). A classifier can pass by
withholding low-confidence errors even when its overall accuracy is poor.

## Inputs

Rows use the calibration report's labels, confidence, question, backend, and
abstention fields. Every row is validated by that existing boundary.

| Option | Default | Domain |
|---|---|---|
| `thresholds` | Required | Nonempty list of finite probabilities in `[0, 1]`. |
| `target_error` | `0.05` | Strictly between zero and one. |
| `delta` | `0.05` | Family-wise failure probability, strictly between zero and one. |
| `model_revision` | Empty | Caller-supplied revision identity. |
| `served_model_id` | Empty | Caller-supplied served-model identity. |

Fix the model and threshold grid before observing calibration labels or
confidence scores. Calibration rows must be IID from the deployment
distribution. Include every consequence tier as a separate `question_id` in
one report. Calling the function separately for newly selected groups spends
additional statistical error budget.

## Output

The `harn.selective_risk.v1` report carries input and report digests, model
identity, `target_error`, `delta`, and `family_size`. The family size is the
number of thresholds multiplied by the number of question/backend groups.
Duplicate thresholds count as additional tests, conservatively.

Each group's `candidates` records `accepted`, `errors`, `coverage`, the
one-sided `error_upper_bound`, and `certified`. Its `recommendation` is either
a `threshold` with those counts and bound, or `no_threshold` with reason
`no_accepted_rows` or `risk_not_certified`. No accepted rows means a bound of
one, never evidence of zero risk. Invalid inputs return `kind: "refused"`.

## Statistical contract

Each fixed threshold uses a one-sided Clopper-Pearson bound obtained by
inverting the exact binomial lower tail. Each bound receives
`delta / family_size`; Bonferroni gives simultaneous coverage of all bounds
with probability at least `1 - delta`. Selection among these bounds therefore
retains that confidence guarantee. The implementation keeps the upper
bisection endpoint plus a conservative numerical guard and never rounds a
bound down before certification.

This is the finite-family multiple-testing construction of
[Learn then Test](https://arxiv.org/abs/2110.01052), using conservative exact
binomial tests. It is not an unconditional guarantee under distribution shift.
The model revision, labels, and deployment representativeness remain caller
responsibilities. Empty identity strings do not certify a stable model alias.

Changing only `target_error` reuses the same simultaneous bounds; act and
verify risk budgets over the same family need no further correction.
Loosening the error target cannot increase the selected threshold. Evaluate
the chosen threshold on a disjoint holdout; do not tune the threshold grid or
target against that holdout and continue claiming its independence.
