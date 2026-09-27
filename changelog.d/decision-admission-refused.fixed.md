- A native decision refused by local spend admission, such as one whose
  `run_cost_limit` arrives after an earlier unbudgeted model call, now returns
  `unavailable` with reason `admission_refused` instead of `authority_denied`.
  Its receipt records the cause in `admission_reason`.
