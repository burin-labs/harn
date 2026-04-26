---
name: release-harn
description: Alias for the Harn release workflow skill.
---

# Release Harn

Use the same workflow as [`harn-release`](../harn-release/SKILL.md).

The release is **one** human PR titled `Release vX.Y.Z` carrying
changelog + code + docs + Cargo.toml bump together. After it lands
through the merge queue, two GitHub Actions workflows cascade
automatically under the `harn-release-bot` App identity:

```text
land "Release vX.Y.Z" → publish-release pushes tag + cargo publish + GH release
        → tag push → build-release-binaries assembles binaries + GHCR container
```

The repo source of truth (only invoke locally for recovery):

```bash
./scripts/release_ship.sh --prepare --bump <patch|minor|major>   # default flow
./scripts/release_ship.sh --finalize                              # recovery
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
```

**Cross-repo consumers do not wait on releases.** `burin-code`'s
`scripts/fetch-harn.sh --local` builds Harn from `~/projects/harn` and
installs the binaries directly — use it during cross-repo iteration
instead of waiting for crates.io. Release batching is a published-version
concern, not a developer-loop concern.

Before opening the release PR, make sure the local developer workflow and
observability surface are documented coherently:

- `README.md`
- `CONTRIBUTING.md`
- `docs/src/portal.md`
- `scripts/dev_setup.sh`
- `Makefile`
- `.githooks/`

Commit pattern for a real release:

1. **`Release vX.Y.Z`** — code + docs + `CHANGELOG.md` + Cargo.toml /
   Cargo.lock + per-crate manifest bumps + regenerated mirrors. Authored
   by you via `release_ship.sh --prepare --bump <type>`, landed through
   PR/merge queue.

That's it. The bot takes over once it lands.

Workflows:

- `.github/workflows/publish-release.yml` (display name: "Publish release")
  — fires on push to main when `Cargo.toml` is ahead of the latest
  `vX.Y.Z` tag (i.e. tag drift). Pushes the tag using the App token so
  downstream cascades fire (a `GITHUB_TOKEN` tag push would be
  suppressed by GHA).
- `.github/workflows/build-release-binaries.yml` (display name: "Build
  release binaries") — fires on tag push. Also accepts a `tag` input via
  `workflow_dispatch` for re-running against an existing tag.
- `.github/workflows/bump-release.yml` (display name: "Open version bump
  PR (recovery)") — workflow_dispatch only, used to reconstruct a bump
  PR if a "Prepare vX.Y.Z release"-style commit accidentally lands on
  main without the consolidated bump.

All three expose `workflow_dispatch` for manual recovery. `gh workflow
run <name> --ref main` re-fires.

**Never push to a PR that's already in the merge queue** — GitHub
silently snapshots the PR at enqueue time and ignores subsequent
pushes. The pre-push hook detects this and aborts.

Required repo state:

- Secrets: `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`,
  `CARGO_REGISTRY_TOKEN`.
- App permissions on the repo: `Contents: write`, `Pull requests:
  write`, `Actions: write`, `Metadata: read`.
