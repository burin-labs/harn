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

Verify the package with `harn package verify . --strict`. No GitHub credential
is needed; the tests use exact typed HTTP fixtures.
