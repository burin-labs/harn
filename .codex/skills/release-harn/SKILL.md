---
name: release-harn
description: Alias for the Harn release workflow skill.
---

# Release Harn

Use the same workflow as [`harn-release`](../harn-release/SKILL.md).

The release is **one** human PR titled `Release vX.Y.Z` carrying
changelog + code + docs + Cargo.toml bump together. After it lands
through the merge queue, **push the `vX.Y.Z` tag at the release commit** — that
tag push (not the merge) drives the rest:

```text
land "Release vX.Y.Z" PR  →  push signed vX.Y.Z tag at the release commit
   (release_harn.harn --mode ship-pr does the tag push; or do it by hand)
        → TAG push → publish-release runs cargo publish + GH release
        → tag push → build-release-binaries assembles binaries + GHCR container
```

> **`publish-release.yml` does NOT tag `main` HEAD** (changed with the
> release-pipeline modernization, #2971–#2973). A missing `vX.Y.Z` tag makes the
> post-merge run fail with `… but vX.Y.Z does not exist. Push vX.Y.Z at the
> release commit`. Push it after merge with
> `git tag -s vX.Y.Z $(git rev-parse origin/main) -m "Release vX.Y.Z" && git push origin vX.Y.Z`.

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
   by you via `release_ship.sh --prepare --bump <type>`, then **rebased
   onto latest `origin/main` before push** (because `--prepare` takes
   1-15 min and main may have moved), landed through PR/merge queue
   with `gh pr merge --auto` enabled so it lands as soon as CI is green.

2. **Push the `vX.Y.Z` tag at the release commit** once the PR lands — this is
   the step that ships. The bot does NOT auto-tag. Use `release_harn.harn --mode
   ship-pr` (which pushes the tag for you) or push it by hand after merge.

Workflows:

- `.github/workflows/publish-release.yml` (display name: "Publish release")
  — publishes on the `vX.Y.Z` **tag push** (`tags: ['v*']`), running
  `cargo publish` from the tag. Its `push: branches:[main]` trigger is only a
  **guard**: it errors if `Cargo.toml` is ahead of the latest tag but the
  matching tag is missing or points at a different commit — it does **not** tag
  `main` HEAD. Push the tag with the App token or your own creds (a
  `GITHUB_TOKEN` tag push would be suppressed by GHA, so downstream wouldn't
  fire).
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
