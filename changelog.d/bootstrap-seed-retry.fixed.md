Retry the stage-zero seed download within the configured
`HARN_BOOTSTRAP_MAX_ATTEMPTS` budget, so a transient release-host error no
longer fails `setup-harn` while it resolves a release. The action now passes
`max-attempts` and `retry-delay-seconds` to that step too, and an invalid
budget is an error instead of a silent fallback to the default.
