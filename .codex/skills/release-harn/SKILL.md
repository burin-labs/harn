---
name: release-harn
description: Alias for the Harn release workflow skill.
---

# Release Harn

Use the same workflow as [`harn-release`](../harn-release/SKILL.md).

The repo source of truth remains:

```bash
./scripts/dev_setup.sh
./scripts/release_ship.sh --bump patch                     # open bump PR
./scripts/release_ship.sh --finalize                       # tag/publish after merge
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...  # piecewise
```

Direct pushes to `main` are not part of the release flow. After the release
content PR lands through the merge queue, `release_ship.sh --bump patch` runs
audit → dry-run publish → bump → commit → push `release/vX.Y.Z` → open a bump
PR. After the bump PR lands, `release_ship.sh --finalize` runs audit → dry-run
publish → tag → push tag → `cargo publish` → render notes → create/update the
GitHub release. The tag push happens before `cargo publish` so downstream
consumers (e.g. `burin-code`'s `fetch-harn`, GitHub release-binary workflows)
can start working in parallel with crates.io.

**Merge-queue flow:** land a "Prepare vX.Y.Z release" PR first, run
`release_ship.sh --bump patch` from updated `main` to open the automated bump
PR, then run `release_ship.sh --finalize` from updated `main` after the bump PR
lands. Check each exit code when it returns; otherwise do not babysit.

**Cross-repo consumers do not wait on releases.** `burin-code`'s
`scripts/fetch-harn.sh --local` builds Harn from `~/projects/harn` and
installs the binaries directly — use it during cross-repo iteration
instead of waiting for crates.io. Release batching is a published-version
concern, not a developer-loop concern.

Before releasing, make sure the local developer workflow and observability
surface are documented coherently:

- `README.md`
- `CONTRIBUTING.md`
- `docs/src/portal.md`
- `scripts/dev_setup.sh`
- `Makefile`
- `.githooks/`

Commit pattern for a real release:

1. `Prepare vX.Y.Z release` — code + docs + `CHANGELOG.md`, no Cargo.toml,
   landed through PR/merge queue.
2. `Bump version to X.Y.Z` — created automatically by `release_ship.sh --bump
   patch`, landed through PR/merge queue.
