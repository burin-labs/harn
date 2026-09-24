# What a confidence score means

A classifier that reports a confidence is making a claim about itself: when it
says 0.9, it is right about nine times in ten. That claim is testable, and
nothing in any model API enforces it.

This page explains why Harn treats a confidence number as unmeasured until a
calibration report says otherwise. For the API and the flags, read
[`std/eval/calibration`](../eval-calibration-reference.md).

## A confidence is usually a shape, not a frequency

Most decision APIs compute confidence from the shape of the answer
distribution. A common form is `(3 * p_max - 1) / 2`: how far the top
probability sits above a uniform guess. That is a well-defined number, and it
has no necessary relationship to how often the answer is right.

A model that is wrong in a consistent, confident way produces a high shape
statistic on every wrong answer. Nothing in the response says so. The API cannot
tell you, because the API never sees the correct answer.

The only thing that can tell you is a corpus where the correct answer is already
known.

## What the report actually measures

Bin the answered rows by stated confidence. In each bin, compare the mean stated
confidence with the observed accuracy. A calibrated classifier sits on the
diagonal: the 0.9 bin is right 90 percent of the time. The expected calibration
error is the row-weighted mean gap between the two.

A calibration error near zero means the numbers can be read as probabilities. A
calibration error near one means the classifier is confident and wrong, and the
confidence field is worse than useless, because a gate reading it will open
exactly when it should not.

## What it does not measure

- **Whether the labels are right.** The report measures agreement with the
  corpus, and inherits every mistake in it.
- **Whether the corpus resembles production.** A classifier calibrated on clean
  synthetic questions can be wildly miscalibrated on real traffic. The
  conformal guarantee assumes the new rows are exchangeable with the corpus,
  which is an assumption about your data, not a property of the method.
- **Whether it is still true.** A provider alias that re-points to a new served
  model invalidates every number. This is why the report carries a served model
  identity and a digest instead of a bare percentage.
- **Anything about one particular row.** Every number here is an average over
  rows. A 3 percent error rate at a threshold is not a promise about the next
  answer.

## Accuracy is the wrong single number

Accuracy folds two failures that are not interchangeable.

A **false accept** is a wrong answer the gate let through. If the question
gates a destructive action, one is an incident.

A **false reject** is a right answer the gate threw away. That is the gate
breaking legitimate work, which is usually the reason someone wanted an
automatic decision in the first place.

A gate that withholds every answer has a perfect false-accept rate. The report
never averages the two. It prints both, at every candidate threshold, each with
its own denominator, because the tradeoff between them is the whole decision.

## Why the threshold is derived, not picked

A hand-picked threshold is a preference dressed as a measurement. Someone looks
at a table, sees that 0.9 looks safe, and writes 0.9 into a policy.

The conformal recommendation derives the threshold from a target error rate. You
say what error rate the gate may have; the report says what threshold reaches it,
or refuses to name one. The threshold is fitted on one half of the corpus and
measured on the other, so the reported error is a number the threshold did not
get to tune against. Fitting and measuring on the same rows always looks better
than it is.

When the classifier cannot reach the target at any threshold, the report says so
with a typed refusal. It never returns a threshold of zero, which would accept
everything and read, to a consumer checking only that a number came back, as a
working gate.

## A probabilistic answer never outranks deterministic policy

A calibration report can license reading a confidence as a probability. It
cannot promote a model's answer above a rule. Where a deterministic policy has
an answer, that answer wins, and the confidence number decides only whether the
model is allowed to speak at all.
