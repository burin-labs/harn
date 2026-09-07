`agent_loop` now refuses an option key nothing on its surface declares, instead
of accepting it in silence. A misspelled key and a real key at a depth where
nothing reads it were both accepted, and the run then reported exactly what a
correctly configured run without that option would report. The two outcomes
were indistinguishable, so a probe written to catch a missing mechanism could
measure its absence and pass.

The rejection names where the key is read when it is read somewhere else, which
is the case a caller cannot debug from a refusal alone: learning that
`requirement_contract` is not a top-level option does not tell you it belongs on
the judge config.

The allowed keys are derived from the type that declares the option surface
rather than listed beside the validators, so they cannot drift from the
declaration the way the per-validator field reads already had.

Turning the check on found options the loop reads that the type never declared,
now declared, and a handful of keys nothing reads at all, now removed: a
workflow stage set three loop-detection values that no code anywhere consumes,
and two conformance tests disabled a judge under a name that was renamed away.
`done_judge` now refuses with its replacement named, `turn_end_condition`,
rather than falling through to the generic "no option reads this".
