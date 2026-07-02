Harden waitpoint replay tests against parallel `cargo test` by replacing the
process-wide `HARN_REPLAY` toggle with a scoped in-process replay override.
