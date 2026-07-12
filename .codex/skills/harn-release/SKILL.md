---
name: harn-release
description: Use for Harn release prep, version bumps, publishing, tags, and release notes.
---

# Harn release

Use the tag-first release flow. The canonical driver is
`~/projects/harn-bump-fleet/release_harn.harn`; local scripts in this repo are
building blocks and recovery tools.

```bash
cd ~/projects/harn-bump-fleet
harn run --no-sandbox release_harn.harn -- \
  --repo ~/projects/harn --mode ship-pr --agent --yes-live-release
```

`ship-pr` prepares the release content, commits it, pushes the branch, pushes
the signed `vX.Y.Z` tag at the pinned release commit, opens the `Release
vX.Y.Z` PR, and enables auto-merge. The tag is pushed before the PR merges so
crates.io, release notes, binaries, and the container are built from the pinned
tag commit, not from whatever is on `main` later.

## Flow

```text
release_harn.harn --mode ship-pr
  -> prepare release content and version bump
  -> commit "Release vX.Y.Z"
  -> push branch
  -> push signed vX.Y.Z tag at the pinned commit
  -> open Release vX.Y.Z PR and enable auto-merge
  -> tag push triggers publish-release and build-release-binaries
```

The Release PR remains the review and merge-queue artifact. Publishing is keyed
to the tag. The later push to `main` is a guard/self-heal path; it must not be
treated as the publishing trigger.

## Source of truth

- `~/projects/harn-bump-fleet/release_harn.harn` owns the live release
  orchestration. Run it from the `harn-bump-fleet` checkout so its prompt
  assets resolve from that repo.
- `scripts/release_ship.sh --prepare` is an implementation detail for the
  release harness and refuses standalone use.
- `scripts/release_ship.sh --finalize` is run by
  `.github/workflows/publish-release.yml` on a tag push. Run it locally only for
  recovery.
- `scripts/release_ship.sh --bump <patch|minor|major>` and
  `.github/workflows/bump-release.yml` are recovery paths for historical
  two-step releases.
- `scripts/release_gate.sh <audit|prepare|publish|notes|full>` provides local
  audit, dry-run, notes, and recovery helpers.

## Before shipping

1. Start from a clean harn worktree and fetch `origin/main`.
2. Inspect pending release content with `git status --short`,
   `git diff --stat`, and enough diff context to summarize it accurately.
3. Audit the changed behavior. Add Rust tests or `.harn` + `.expected`
   conformance pairs for new user-visible behavior.
4. Run targeted tests while fixing issues. Before live release mechanics, run
   the gates that cover the touched surface; broad releases normally need
   `make test` and `cargo run --bin harn -- test conformance`.
5. Update release-facing docs when the behavior changed: `README.md`,
   `AGENTS.md` / `CLAUDE.md`, `CONTRIBUTING.md`, `docs/src/`,
   `spec/chapters/*.md`, and generated mirrors through their generators.

## Workflows

- `.github/workflows/publish-release.yml` ("Publish release") fires on
  `v*` tag pushes, checks out the tag, and runs
  `./scripts/release_ship.sh --finalize` under the release App identity. Its
  `push: main` trigger is a guard; it does not tag `main` for you.
- `.github/workflows/build-release-binaries.yml` ("Build release binaries")
  fires on the tag push and produces binary tarballs plus the GHCR container.
  Use `workflow_dispatch` with `tag=vX.Y.Z` to recover an existing tag.
- `.github/workflows/bump-release.yml` ("Open version bump PR (recovery)") is
  manual-only recovery for accidental historical release states.
- `.github/workflows/release-pr-drift-check.yml` can ask you to rerun
  `release_harn.harn` when a release PR's pin diverges from `origin/main`.

## Recovery

- Finalize failed after the tag exists: rerun `publish-release.yml` from the
  Actions UI or with `gh workflow run publish-release.yml --ref main`.
- Binary assets failed for an existing tag:
  `gh workflow run build-release-binaries.yml --ref main -f tag=vX.Y.Z`.
- Historical prepare landed without the consolidated bump:
  `gh workflow run bump-release.yml`.
- Local recovery only: use `scripts/release_ship.sh --finalize` from the
  correct tag checkout or updated `main` after reading the script help.

## Rules

- Do not hand-run `release_ship.sh --prepare` for the default release path; use
  `release_harn.harn --mode ship-pr`.
- Do not push to a PR already in the merge queue. The pre-push hook checks this
  because GitHub snapshots queued PRs.
- Do not pass `--squash`, `--merge`, or `--rebase` to `gh pr merge --auto`;
  branch protection chooses the strategy.
- Do not hand-edit generated files. Edit sources and regenerate.
- Cross-repo consumers do not wait on a release. For `burin-code`, use
  `./scripts/fetch-harn.sh --local` in that repo to build from
  `~/projects/harn` during iteration.

Required release infrastructure: release App permissions `Contents: write`,
`Pull requests: write`, `Actions: write`, `Metadata: read`; repo secrets
`RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`, and `CARGO_REGISTRY_TOKEN`.
