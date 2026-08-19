# Pre-registration: 36-run diagnostics timing matrix

Written and committed **before the first matrix trial runs**. Nothing in this
file may be edited once trials begin; corrections go in the writeup as
amendments, dated, with the reason.

The point of writing it first is narrow and specific. One fixture is already
known to fail, and it fails for a reason that has nothing to do with what this
experiment measures. If that were sorted out after the results arrived, the
sorting would be indistinguishable from fitting a story to the numbers.

## What the matrix measures

Six timing arms (`lib/policy.harn`) against the fixture set
(`lib/fixtures.harn`), on the json tool channel resolved from the model
catalog. The question is **when** parser and AST findings should reach a coding
agent, holding the finding producer and the precision tiers fixed across every
arm.

## Registered channel and provenance

The tool channel is read from the generated model catalog at run time. It is not
pinned by hand and not taken from a remembered description of the route. Every
trial row stamps a `provenance` record carrying the resolved tool channel, the
resolved endpoint, the resolved model, the resolved request timeout, and the rig
SHA, so a run that drifted onto a different configuration is visible in the data
rather than reconstructed from memory afterwards.

Calibration on this stack resolved to `json`. Native tool calling is
**unmeasurable** here, not merely worse: the catalog declares
`native_tools = false` with `tool_mode_parity = "text_only"`, and a forced
native request ships zero tool schemas, so the agent loop cannot run at all. The
matrix result is a json-channel result and the writeup says so.

## Registered known-stalling cell: `rename-single-file`

`rename-single-file` is **expected to fail on every arm**, and it stays in the
matrix.

Observed shape, from 2 out of 2 json calibration runs before any matrix trial:

- The run stops at **iteration 2 to 3**.
- The model **emits narration** describing the rename it is about to perform.
- The model **emits no mutating tool call**. `tool_sequence` contains reads and
  no `edit_file`.
- Termination is **natural**: the agent stops because it decided it was
  finished, not because it hit the iteration ceiling and not because of an
  error.
- `passed` is false.

This is a defect in the **tool-call emission stream**. It matches a standing
model and harness behaviour class already recorded elsewhere, it reproduced
before the current work started, and it is not this experiment's to fix. Chasing
it would be scope creep.

It is not dropped, because dropping it would hide a real workload failure and
would narrow the measured set until the matrix could only report success.

## Registered scoring rule

`analyze.harn` classifies every trial into exactly one of three classes. The
classifier is structural, not a threshold fitted to the observed runs:

| Class | Definition |
| --- | --- |
| `resolved` | the trial passed |
| `tool_emission_stall` | the trial did not pass, the agent self-terminated, and it emitted no mutating tool call |
| `unresolved` | any other non-passing trial |

`tool_emission_stall` is **excluded from the pass rate, from the turns-to-green
mean, and from the paired bootstrap intervals**, and reported in its own column.
The reasoning is the same one that keeps an infrastructure skip out of a failure
count: a trial that never attempted the task gave no diagnostic timing policy
anything to act on, so charging it to the post-edit diagnostics path would blame
the resolution work for a model behaviour it does not touch.

Note the deliberate looseness. The classifier does **not** key on "iteration 2
to 3". The expected shape above is registered so the prediction can be checked;
the classifier is structural so that a stall arriving at iteration 9 is still
scored as a stall rather than quietly counted as a resolution failure.

## Registered reporting rule

Results are reported **per fixture**, not only in aggregate, and the worst
fixture is named explicitly.

A fixture whose outcome is identical on every arm is a **constant** and
discriminates nothing. `analyze.harn` labels it as such on its own line. On
`rename-single-file` specifically the stall is expected to hit all six arms
equally, so that fixture is predicted to be a constant and to contribute no
information about timing. An aggregate that averaged a constant in would
overstate what was measured.

## Registered falsifiers

- **The rig's own falsifier.** `clean-control` produces no diagnostics, so the
  arms must not differ on it. If they do, the rig has an artifact and the race
  is invalid regardless of what the other fixtures show.
- **The stall prediction.** If `rename-single-file` does **not** stall, or
  stalls on only some arms, this pre-registration was wrong and the writeup says
  so before it says anything else.
- **The separation claim.** A paired 95% bootstrap interval spanning zero is no
  separation. Ranking arms by mean where the interval spans zero is not allowed.

## Known blind spots, registered in advance

- Results cover one model on one serving stack and one tool channel. They do not
  generalise across tool formats, and the native channel could not be measured
  at all.
- Mock mode cannot measure behaviour; a scripted model does not read what it is
  shown. Every behavioural number comes from live mode.
- The stall removes `rename-single-file` from the discriminating set, so the
  mid-flight-window question is carried by `rename-cross-file` alone unless the
  stall clears.
