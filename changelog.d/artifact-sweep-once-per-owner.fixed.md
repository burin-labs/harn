`run_command` no longer re-sweeps the shared command-artifact directory on every
call when other live sessions hold enough directories to keep it over its cap.
A sweep that ends over the cap waits for the normal interval, and each sweep
checks an owner's session lease once instead of once per directory.
