- `register_session_end_hook` and `register_harn_owned_pid` now return RAII
  registrations that callers must retain for the desired lifetime.
  Package-generation reader leases left open by an aborted run are released
  during runtime reset.
