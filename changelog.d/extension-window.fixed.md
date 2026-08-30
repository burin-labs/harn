The adaptive iteration budget now decides extension from a window of recent
turns instead of the single turn the budget boundary lands on. A run that had
been editing and then spent its last turns reading files back to repair a
failed check was stopped at exactly its initial cap, because that one boundary
turn was suppressed by `no_net_advance` or `no_information_gain` while the
repair evidence was a few turns old.

The rule uses the same definition of progress as before — a turn counts only if
it satisfies `agent_loop_is_progressing` — and looks back `extend_by` turns by
default. `iteration_budget.progress_window` sets the width explicitly. A window
in which no turn satisfies the predicate still stops the loop, and
`iteration_budget.max` remains the outer bound. The recorded decision reason is
`progress within window` and appears under `adaptive_budget.decisions` when
`expose_decisions` is set.
