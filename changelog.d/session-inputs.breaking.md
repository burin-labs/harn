- **Named private session inputs (#10222).** Hosts can supply secrets and
  ordinary variables with expiry, exact HTTPS website limits, and output
  filtering. `JobRunOptions` now includes `session_inputs` for private job grants.

  Migration: use `JobRunOptions::default()` or `JobRunOptions::fail_fast()`.
  Existing struct literals must add `session_inputs: None`.
