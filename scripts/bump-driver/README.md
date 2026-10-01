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
- The workflow supplies renewable App credentials to the driver. Its connector
  mints tokens for only the calling repository, with Contents: write and Pull
  requests: write. REST requests and Git fetches share token renewal, so a long
  validation cannot leave publication using an expired bootstrap token.

`GithubBumpRemoteConfig.options` accepts `GithubClientOptions`. The legacy
`token` field remains available for callers that manage their own token
lifetime; supply one field, not both. The locked connector requires Harn
0.10.135 or newer. App credentials stay inside the driver; caller refresh
commands receive only a scoped token through `GH_TOKEN`.
Each new refresh callback receives a current token. A child command making
GitHub calls beyond that token's lifetime must manage its own renewal.

Verify the package with `harn package verify . --strict`. No GitHub credential
is needed; the tests use exact typed HTTP fixtures.
