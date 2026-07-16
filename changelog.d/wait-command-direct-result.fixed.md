`wait_command(timeout_ms)` now synchronizes directly with live background
handles, so it can return a completed result after process output is visible but
before the session feedback inbox wakes.
