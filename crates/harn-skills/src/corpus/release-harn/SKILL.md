---
name: release-harn
short: Merge-queue-safe Harn patch/minor/major release workflow.
description: Cut a Harn release through the merge queue. One PR carries CHANGELOG + Cargo.toml bump + regenerated artifacts; downstream tag push + binary build cascades fire automatically once it lands.
when_to_use: Use when cutting a Harn `vX.Y.Z` patch / minor / major release from main, or recovering from a partially-failed release run.
---

# Release Harn

Use this skill when cutting a Harn `vX.Y.Z` release from `main`, or
recovering from a partial release.

Pair it with [[harn-providers]] when the release includes provider
catalog or capability matrix changes (the bump regenerates those
artifacts).

## Shape of a release

A Harn release is **one human PR** titled `Release vX.Y.Z` carrying
the consolidated bump:

- `CHANGELOG.md` — a new `## vX.Y.Z` section at the top with
  `### Added` / `### Changed` / `### Fixed` subsections summarising
  everything that lands in this version.
- `Cargo.toml` (workspace `version` field) bumped to `X.Y.Z`.
- `Cargo.lock` re-locked.
- Any per-crate manifest bumps the release-prep script touches
  (`crates/*/Cargo.toml`).
- Regenerated mirror artifacts the release-prep script refreshes
  (provider catalog JSON / TS / Swift, capability matrix, highlight
  keywords, etc. — whatever the previous release shipped).

After this PR lands through the merge queue, two GitHub Actions
workflows cascade automatically under the `harn-release-bot` App
identity:

```text
land "Release vX.Y.Z" → publish-release pushes tag + cargo publish + GH release
        → tag push → build-release-binaries assembles binaries + GHCR container
```

## Local entry points

The default flow:

```bash
./scripts/release_ship.sh --prepare --bump <patch|minor|major>
```

Recovery / partial-run reentry:

```bash
./scripts/release_ship.sh --finalize
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
```

Manual workflow_dispatch entry points for recovery:

```bash
gh workflow run publish-release.yml --ref main
gh workflow run build-release-binaries.yml --ref main -f tag=vX.Y.Z
gh workflow run bump-release.yml --ref main          # reconstruct a missed bump PR
```

## Before opening the PR

Sanity-check the local developer surface so any release-note bump that
mentions setup still works:

- `README.md`
- `CONTRIBUTING.md`
- `docs/src/portal.md`
- `scripts/dev_setup.sh`
- `Makefile`
- `.githooks/`

## Commit pattern

A real release lands as **one** commit on `main` after squash:

1. `Release vX.Y.Z` — code + docs + `CHANGELOG.md` + Cargo.toml /
   Cargo.lock + per-crate manifest bumps + regenerated mirrors.
   Authored by you via `release_ship.sh --prepare --bump <type>`,
   then **rebased onto latest `origin/main` before push** (because
   `--prepare` takes 1–15 min and main may have moved during),
   landed through PR / merge queue with `gh pr merge --auto`
   enabled so it lands as soon as CI is green.

That's it. The bot takes over once it lands.

## Cross-repo consumers don't wait on releases

An IDE host's `scripts/fetch-harn.sh --local` builds Harn from
`~/projects/harn` and installs the binaries directly. Use that during
cross-repo iteration instead of waiting for crates.io. Release batching
is a published-version concern, not a developer-loop concern.

## Workflows

- `.github/workflows/publish-release.yml` (display name: "Publish
  release") — fires on push to main when `Cargo.toml` is ahead of
  the latest `vX.Y.Z` tag. Pushes the tag using the App token so
  downstream cascades fire (a `GITHUB_TOKEN` tag push would be
  suppressed by GHA).
- `.github/workflows/build-release-binaries.yml` (display name:
  "Build release binaries") — fires on tag push. Also accepts a
  `tag` input via `workflow_dispatch` for re-running against an
  existing tag.
- `.github/workflows/bump-release.yml` (display name: "Open
  version bump PR (recovery)") — `workflow_dispatch` only; used to
  reconstruct a bump PR if a "Prepare vX.Y.Z release"-style commit
  accidentally lands on main without the consolidated bump.

## Hard rules

- **Never push to a PR already in the merge queue.** GitHub silently
  snapshots the PR at enqueue time and ignores subsequent pushes.
  The pre-push hook detects this and aborts.
- **Always rebase the release PR onto latest `origin/main` before
  the final push.** `release_ship.sh --prepare` takes long enough
  that main usually moves; not rebasing means the merge queue sees
  a stale base and reorders the bump behind unrelated work.
- **Never amend the release commit after CI starts.** Open a new
  bump PR via the recovery workflow instead.

## Required repo state

- Secrets: `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`,
  `CARGO_REGISTRY_TOKEN`.
- App permissions on the repo: `Contents: write`, `Pull requests:
  write`, `Actions: write`, `Metadata: read`.

## Verify

- The release bump compiles: `cargo check --workspace`.
- Conformance still passes: `make conformance`.
- Catalog artifacts are in sync: `harn providers validate
  --check-artifacts`.
- The CHANGELOG entry is non-empty and cites the issue / PR numbers
  the release contains.
- `Cargo.toml` workspace `version` matches the PR title's `vX.Y.Z`.
