---
name: harn-release
description: Use this skill for Harn release prep, version bumps, publishing, tagging, and release notes.
---

# Harn release gate

The release is **one** human PR titled `Release vX.Y.Z`. It carries the
changelog, code, docs, AND the `Cargo.toml`/`Cargo.lock` bump together.
After it lands through the merge queue, push the `vX.Y.Z` **tag** at the
release commit — *that tag push* (not the merge to `main`) triggers the
**publish-release** workflow to `cargo publish`, and then the
**build-release-binaries** workflow for binary tarballs and a multi-arch
container.

> **`publish-release.yml` does NOT tag `main` HEAD for you** (changed with the
> release-pipeline modernization, #2971–#2973). A missing tag makes the
> post-merge run fail: `Cargo.toml=X is ahead of latest tag vY, but vX does not
> exist. Push vX at the release commit`. The canonical orchestrator
> (`release_harn.harn --mode ship-pr`) pushes the tag for you; otherwise push it
> by hand (step 9 / 10).

```text
human/agent: write & land "Release vX.Y.Z" PR
        │  ↓ merge queue runs full audit set in CI
human/agent: push signed vX.Y.Z tag at the release commit
        │   (orchestrator ship-pr does this; or push it by hand)
        │  ↓ the TAG push (not the main push) triggers publish-release
bot:    publish-release runs cargo publish, creates GH release notes
        │  ↓ tag push cascades
bot:    build-release-binaries workflow assembles binaries + container
        │  ↓
        v0.8.X is shipped (binaries, container, crates.io, release notes)
```

The bot workflows live at:

- `.github/workflows/publish-release.yml` (display name: "Publish release")
- `.github/workflows/build-release-binaries.yml` (display name: "Build release binaries")
- `.github/workflows/bump-release.yml` (display name: "Open version bump PR (recovery)" — workflow_dispatch only)

## Source of truth

All bot workflows invoke the same scripts you'd run locally:

```bash
./scripts/release_ship.sh --prepare --bump <patch|minor|major>   # default flow
./scripts/release_ship.sh --finalize                              # recovery
./scripts/release_ship.sh --bump <patch|minor|major>              # legacy recovery
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
```

Do not re-invent the release ritual from memory if the script can do it.

## Single-PR mode

A real release has exactly one release commit on `main`, landed via
PR/merge queue: `Release vX.Y.Z`. It contains code, docs,
`CHANGELOG.md`, `Cargo.toml` / `Cargo.lock` and per-crate manifest
bumps, and regenerated derived files (highlight keywords,
language-spec mirror).

After the release PR lands, **push the `vX.Y.Z` tag at the release commit**
(step 10) — the publish and binary-build workflows fire off the *tag push*, not
the merge. Wall-clock ~3-5 min for crates.io publish + ~10-15 min for binary
tarballs.

Failure modes, roughly in frequency order, with recovery:

- `release_gate.sh audit` clippy / test failure during `--prepare`:
  fix the code on the same release branch, re-run `--prepare
  --skip-audit` if you've already run audit successfully once.
- `cargo publish` rate-limit / transient network during the publish
  workflow: re-trigger via `gh workflow run publish-release.yml --ref main`.
  The script falls back to per-crate publish and treats `already
  exists on crates.io index` as success.
- Binary build needs to re-emit for an already-tagged version:
  `gh workflow run build-release-binaries.yml --ref main -f tag=vX.Y.Z`.

## Cross-repo iteration does not wait on releases

Downstream repos (notably `burin-code`) can consume in-progress Harn
changes without a release via `./scripts/fetch-harn.sh --local` in the
consumer repo — it builds Harn from `~/projects/harn` in release mode
and installs the binaries directly. Release batching exists to control
the *published* version surface; it never blocks cross-repo iteration.

## What you actually do for a release

Steps 1-10 are the only ones requiring judgment. Step 10 (pushing the `vX.Y.Z`
tag) is the step that actually ships — the merge alone publishes nothing. After
step 10 you are done — do **not** run `release_ship.sh --finalize` locally as a
default step.

1. Branch off main: `git checkout -b release/vX.Y.Z`.
2. Inspect the worktree first with `git status --short` and
   `git diff --stat`. Treat tracked and untracked changes as candidate
   release content unless the user scopes the release more narrowly.
3. Read enough diff context to summarize the pending work accurately.
4. Audit pending changes for correctness and test coverage. Add Rust
   tests or conformance pairs for new or changed user-visible behavior;
   fix bugs discovered during the audit instead of shipping them.
   - Targeted crate tests during the inner loop (`cargo nextest run -p harn-vm`).
   - `make test` and `cargo run --bin harn -- test conformance` before
     proceeding with release mechanics.
5. Repo-consistency sweep before shipping. Update release-facing docs
   and operator guidance as needed: `README.md`, `CLAUDE.md`,
   `docs/src/`, `spec/HARN_SPEC.md`, `CHANGELOG.md`, and developer-setup
   surfaces (`scripts/dev_setup.sh`, `Makefile`, `.githooks/`,
   `docs/src/portal.md`).
6. If syntax / parser / lexer / tree-sitter changed, update
   `spec/HARN_SPEC.md` first — formal language-spec source of truth.
   The pre-commit hook regenerates `docs/src/language-spec.md`
   automatically; CI gates on it via `make check-language-spec`.
7. Update `CHANGELOG.md` with a new top entry `## vX.Y.Z` describing
   the actual pending code changes that will ship. The version chosen
   here drives what `--prepare` will bump to.
8. Run the consolidated prep:

   ```bash
   ./scripts/release_ship.sh --prepare --bump patch
   ```

   This audits, dry-run-publishes, bumps `Cargo.toml`/`Cargo.lock`/per-crate
   manifests, regenerates derived files, and `git add`s everything.
9. Commit, rebase onto latest `origin/main`, push, open the PR titled
   `Release vX.Y.Z`, then `gh pr merge --auto`.

   ```bash
   git commit -m "Release vX.Y.Z"
   git fetch origin main && git rebase origin/main
   # Resolve any CHANGELOG.md conflicts: bullets that landed on main
   # while --prepare was running may need to move from v(X.Y.Z-1) → vX.Y.Z,
   # or a bullet may end up duplicated across both sections. Verify with:
   #   diff <(git show v(X.Y.Z-1):CHANGELOG.md | awk '/^## v(X.Y.Z-1)/,/^## /') \
   #        <(awk '/^## v(X.Y.Z-1)/,/^## /' CHANGELOG.md)
   git push -u origin release/vX.Y.Z
   gh pr create --title "Release vX.Y.Z" --body "..."
   gh pr merge --auto         # branch protection picks the strategy; do
                              # NOT pass --squash/--merge/--rebase
   ```

   Rebase right before push because `--prepare` takes 1-15 min depending
   on cache state, and main may have moved while it ran. Auto-merge means
   the PR lands as soon as CI is green so you don't have to babysit the
   ~10-15 min cold-cache merge-queue CI.

10. **Push the `vX.Y.Z` tag at the release commit — this is what ships.**
    `publish-release.yml` will not tag `main` HEAD; until the tag exists nothing
    publishes. After the `Release vX.Y.Z` PR squash-merges:

    ```bash
    git fetch origin main --tags
    REL=$(git rev-parse origin/main)   # the squashed "Release vX.Y.Z (#N)" commit
    git tag -s vX.Y.Z "$REL" -m "Release vX.Y.Z"   # signed (org rulesets); -a also works
    git push origin vX.Y.Z
    ```

    The tag push triggers `publish-release.yml` (`tags: ['v*']`), which skips
    drift detection and publishes from the tag. `release_harn.harn --mode
    ship-pr` does this for you. A transient red `publish-release` run on the
    `main` push is expected (the "tag missing" guard) — the tag push is the real
    ship signal.

## Expectations

- Stop on the first failed gate during `--prepare`. Do not paper over.
- Once the release PR lands and you push the `vX.Y.Z` tag (step 10), watch the
  Actions UI. The publish → binary-build cascade (off the tag push) should
  complete in ~12-18 min wall-clock total. Each workflow has
  `workflow_dispatch` for manual recovery.
- Treat repo consistency as part of the release PR, not an optional
  cleanup pass. If behavior changes, update human-facing docs in the
  same PR.
- The grammar / spec audit includes `scripts/verify_language_spec.py`
  (extracts ` ```harn ` fences from `spec/HARN_SPEC.md` and runs `harn
  check`) and `scripts/verify_tree_sitter_parse.py` (sweeps positive
  `.harn` programs through the executable tree-sitter grammar). Treat
  failures as spec drift, not just docs drift.
- **Never push to a PR that's already in the merge queue** —
  GitHub silently snapshots the PR at enqueue time and ignores
  subsequent pushes. The pre-push hook detects this and aborts.

## Notes

- `scripts/publish.sh` remains the crates.io publisher. It tries
  `cargo publish --workspace` first with retries, then falls back to
  per-crate publish where `already exists on crates.io index` is
  treated as success.
- `CHANGELOG.md` is the release-language source of truth. Notes are
  rendered from it by `scripts/render_release_notes.py`. CI runs
  `verify_release_metadata.py` to reject malformed headings, empty
  section bodies, or out-of-order entries.
- GitHub release artifacts (binary tarballs + GHCR container) are
  produced by `build-release-binaries.yml` once the tag is pushed (step 10).
  Push the tag with the App identity (`release_harn.harn --mode ship-pr`) or
  your own credentials — a `GITHUB_TOKEN`-pushed tag would NOT trigger the
  downstream workflows.
- The tag is pushed in **step 10** (by `release_harn.harn --mode ship-pr`, or by
  hand), *before* `publish-release.yml` runs `cargo publish`, so the binary-build
  workflow and downstream fetchers (e.g. `burin-code`'s `fetch-harn`) start in
  parallel with crates.io publication. `release_ship.sh --finalize` no longer
  pushes the tag — it publishes from the existing tag.
- `release_ship.sh --finalize` skips the audit by default
  (`RELEASE_FINALIZE_REAUDIT=0`); merge-queue CI of the just-landed
  Release PR proved the same gates a few minutes ago. Pass
  `--reaudit` to opt back in for paranoid local recovery.
- `release_gate.sh audit` (called by `--prepare`) starts with a
  serial `cargo build -p harn-cli --bin harn` warm prebuild before
  spawning the seven parallel lanes. Cold wall-clock is dominated by
  `rust-audit` clippy/nextest plus package verification; warm wall-clock
  should be a few minutes.
- The release-bot App needs `Contents: write`, `Pull requests: write`,
  `Actions: write`, `Metadata: read` installed on this repo. Required
  repo secrets: `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`,
  `CARGO_REGISTRY_TOKEN`.
