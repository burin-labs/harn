ACP now checks declared host operations when a prompt starts. Missing operations
produce `HARN-CAP-008` warnings by default. Set
`require_declared_operations_served = true` to stop before the script runs.
