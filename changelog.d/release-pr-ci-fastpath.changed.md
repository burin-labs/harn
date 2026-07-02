- Release PR CI now skips redundant PR-head Rust, macOS, and Windows lanes
  when the release branch diff contains only generated release metadata, while
  preserving full merge-queue and post-merge backstops.
- Local git hooks no longer treat every Makefile-only change as a Rust
  workspace compile trigger; Makefile changes still run workflow lint and
  generated-artifact registry checks.
- The advisory CLI cold-start budget now skips release PRs, avoiding a
  version-bump-only release binary build that does not gate cold-start changes.
