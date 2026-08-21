# Diagnostics timing matrix: report template

This file is a **template**, not a result. It contains no numbers on purpose, so
nothing in it can be mistaken for a measurement or carried forward as one. Every
`<!-- FILL -->` marker is replaced by output from
`harn run analyze.harn -- <trials.jsonl> --json`, or deleted along with its
section if the section does not apply.

Read [PRE-REGISTRATION.md](PRE-REGISTRATION.md) first. Anything the finished
report says that contradicts the pre-registration is an amendment, and must be
labelled as one, dated, with the reason.

## Headline

State what was measured, and what it does not cover, before any number.

<!-- FILL: one paragraph. Which question the matrix answers, on which tool
     channel, on which model and serving stack, and what it therefore does not
     generalise to. -->

## Did the rig hold?

The falsifier comes before the finding. If `clean-control` shows the arms
differing on any exposure counter, the run measured the rig rather than the
policy under test, and nothing below is worth reading.

<!-- FILL: the analyzer's "Rig falsifier" block verbatim: HELD, VIOLATED, or
     ABSENT, with its detail line. ABSENT is not a pass. -->

## Did the registered prediction hold?

The pre-registration predicted that `rename-single-file` stalls on every arm, at
iteration 2 to 3, with narration emitted and no mutating tool call.

<!-- FILL: whether that prediction held, held partially, or failed. If it
     failed, say so here, before any result, and say what that implies for the
     rest of the reading. -->

## Provenance

<!-- FILL: the analyzer's provenance block: resolved tool channel, resolved
     endpoint, resolved model, resolved request timeout, rig SHA. A field that
     is not uniform across rows means the run drifted mid-matrix, and that is a
     finding rather than a footnote. -->

## Per fixture

The per-fixture table comes before the aggregate, because a fixture that is a
constant contributes no information and averaging it in overstates the result.

<!-- FILL: the analyzer's "By fixture" table. Then, in prose: name the worst
     fixture explicitly, and for every fixture the analyzer marked a constant,
     say plainly that the matrix discriminates nothing there. -->

## Per arm

<!-- FILL: the analyzer's "By arm" table, including the stall column. Turns to
     green are read beside the pass rate, never alone. -->

## Separation

<!-- FILL: the analyzer's paired bootstrap table. An interval spanning zero is
     no separation, and arms are not ranked by mean where it does. Report the
     dropped-pair count too: those are pairs where either side stalled, and a
     large count means the comparison rests on fewer observations than the trial
     count suggests. -->

## What this does not show

Carry the pre-registered blind spots forward, and add any the run itself
revealed.

<!-- FILL: at minimum the tool channel limitation, the unmeasurable native
     channel, and which fixtures dropped out of the discriminating set. -->

## Amendments

<!-- FILL: anything decided after trials began, dated, with the reason. If
     nothing was, write "none" rather than deleting the section, so the absence
     is on the record. -->
