- The `Public metadata privacy` check now runs in its own workflow and is
  declared as a required status check in `scripts/required_checks.toml`, which
  `make check-required-checks` holds to the workflows that report each check.
  Disabling an unrelated gate no longer disables it, a title or body edit
  re-runs it on the head commit, and an event without a title or body key now
  fails as unmeasured instead of passing.
