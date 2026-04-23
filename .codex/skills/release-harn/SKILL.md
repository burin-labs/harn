---
name: release-harn
description: Alias for the Harn release workflow skill.
---

# Release Harn

Use the same workflow as [`harn-release`](../harn-release/SKILL.md).

The **only** human/agent-driven step is writing and landing the
**"Prepare vX.Y.Z release"** PR. After that, three GitHub Actions
workflows cascade automatically under the `harn-release-bot` App
identity:

```text
land "Prepare vX.Y.Z release" → Bump Release opens "Bump version" PR
        → Bump PR lands → Finalize Release tags + publishes crates.io
        → tag push → Release builds binaries + GHCR container
```

The repo source of truth (only invoke locally for recovery):

```bash
./scripts/release_ship.sh --bump <patch|minor|major>   # recovery only
./scripts/release_ship.sh --finalize                   # recovery only
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
```

**Cross-repo consumers do not wait on releases.** `burin-code`'s
`scripts/fetch-harn.sh --local` builds Harn from `~/projects/harn` and
installs the binaries directly — use it during cross-repo iteration
instead of waiting for crates.io. Release batching is a published-version
concern, not a developer-loop concern.

Before opening the prepare PR, make sure the local developer workflow and
observability surface are documented coherently:

- `README.md`
- `CONTRIBUTING.md`
- `docs/src/portal.md`
- `scripts/dev_setup.sh`
- `Makefile`
- `.githooks/`

Commit pattern for a real release:

1. **`Prepare vX.Y.Z release`** — code + docs + `CHANGELOG.md`, no
   Cargo.toml. Authored by you, landed through PR/merge queue.
2. **`Bump version to X.Y.Z`** — `Cargo.toml` + `Cargo.lock` only,
   opened by Bump Release workflow under the bot identity, landed
   through PR/merge queue. Triggers Finalize Release on landing.

Workflows:

- `.github/workflows/bump-release.yml` — fires on `Prepare v` commits.
- `.github/workflows/finalize-release.yml` — fires on `Bump version to`
  commits. Pushes the tag using the App token so downstream cascades
  fire (a `GITHUB_TOKEN` tag push would be suppressed by GHA).
- `.github/workflows/release.yml` — fires on tag push. Also accepts a
  `tag` input via `workflow_dispatch` for re-running against an existing
  tag.

All three expose `workflow_dispatch` for manual recovery. `gh workflow
run <name> --ref main` re-fires.

Required repo state:

- Secrets: `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`,
  `CARGO_REGISTRY_TOKEN`.
- App permissions on the repo: `Contents: write`, `Pull requests:
  write`, `Actions: write`, `Metadata: read`.
