Smart Ship can prepare one signed GitHub candidate from reserved reviewed changes
and recover its exact object after a lost response or controller restart without
signing again. Candidate preparation preserves the destination and release
checkpoint, refuses a stop received during preparation, and shares review and
provider-binding policy with grouped publication.

The GitHub command now uses that grouped route for preparation, publication,
recovery and temporary state-object cleanup, replacing the former per-PR REST
merge writer. Recovery requires the current controller and policy. Policy
identity preserves signatures when a JSON reader reencodes integral spending
budgets as integers instead of floating-point numbers.

Stopping or returning to shadow mode records a terminal refusal for requests
that never acquired a dispatch claim, so cancelled intent cannot block a later
authority transfer. Claimed outcomes remain unresolved until measured.
Temporary state-object cleanup guards the current authority revision in the
same atomic transaction as deletion, preventing cleanup after an accepted stop.
Candidate branch cleanup now requires confirmed integration and measured destination
retention, with authority, destination and exact branch guards in one atomic
transaction shared with integration staging cleanup.
