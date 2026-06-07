- Added a terminal `done_judge.max_invocations` / `max_feedback` cap with
  structured run-record counters so repeated done-judge veto loops can stop as
  `verify_capped` instead of running to the iteration budget.
