# Harn runtime bump driver

This nested package binds Harn's provider-neutral `std/bump` state machine to
the typed `harn-github-connector` package. The reusable bump workflow installs
the checked-in lockfile, then runs `bump_harn_runtime.harn` from the consumer
repository's working directory.

The boundary is deliberate:

- `std/bump/runtime` owns orchestration and receipts.
- `std/bump/live` owns local filesystem, git, command, and polling effects.
- This package maps the remote capability to one locked connector revision.
- `harn-github-connector` owns GitHub transport, exact leases, worktree byte
  encoding, signed publication, tree comparison, and pull-request mutations.
- The driver retries a leased signed-worktree publication up to three times
  only for GitHub 5xx network envelopes. Each replay uses the same checked and
  validated worktree plus the same base lease; refresh and validation are never
  rerun. Authentication, schema, conflict, and other semantic failures remain
  fail-fast.

Verify the package with `harn package verify . --strict`. No GitHub credential
is needed; the tests use exact typed HTTP fixtures.
