---
name: harn-release
description: Use this skill for Harn release prep, version bumps, publishing, tagging, and release notes.
---

# Harn release gate

The release is **one** human PR titled `Release vX.Y.Z`. It carries the
changelog, code, docs, AND the `Cargo.toml`/`Cargo.lock` bump together.
After it lands through the merge queue, the **publish-release**
workflow auto-fires on tag drift, ships to crates.io, and tags
`vX.Y.Z`. The tag push triggers the **build-release-binaries**
workflow for binary tarballs and a multi-arch container.

```text
human/agent: write & land "Release vX.Y.Z" PR
        │  ↓ merge queue runs full audit set in CI
bot:    publish-release workflow auto-fires on tag drift
        │   pushes vX.Y.Z, runs cargo publish, creates GH release notes
        │  ↓ tag push cascades
bot:    build-release-binaries workflow assembles binaries + container
        │  ↓
        v0.7.X is shipped (binaries, container, crates.io, release notes)
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

After the prepare PR lands, watch the Actions runs. The publish and
binary-build workflows fire downstream automatically. Wall-clock
~3-5 min for crates.io publish + ~10-15 min for binary tarballs.

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

Steps 1-9 are the only ones requiring judgment. After step 9 you are
done — do **not** run `release_ship.sh --finalize` locally as a
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
9. Commit + push + open the PR titled `Release vX.Y.Z`. Walk away.

## Expectations

- Stop on the first failed gate during `--prepare`. Do not paper over.
- Once the release PR lands, watch the Actions UI. The publish →
  binary-build cascade should complete in ~12-18 min wall-clock total.
  Each workflow has `workflow_dispatch` for manual recovery.
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
  produced by `build-release-binaries.yml` once the tag is pushed.
  Tag push from `publish-release.yml` uses the App identity, so the
  cascade fires normally — `GITHUB_TOKEN`-pushed tags would NOT
  trigger it.
- `release_ship.sh --finalize` pushes the tag **before** running
  `cargo publish`, so the binary-build workflow and downstream
  fetchers (e.g. `burin-code`'s `fetch-harn`) can start working in
  parallel with crates.io publication.
- `release_ship.sh --finalize` skips the audit by default
  (`RELEASE_FINALIZE_REAUDIT=0`); merge-queue CI of the just-landed
  Release PR proved the same gates a few minutes ago. Pass
  `--reaudit` to opt back in for paranoid local recovery.
- `release_gate.sh audit` (called by `--prepare`) starts with a
  serial `cargo build --workspace --all-targets` warm prebuild before
  spawning the five parallel lanes. Cold ~6-10 min, warm ~10-30 s.
- The release-bot App needs `Contents: write`, `Pull requests: write`,
  `Actions: write`, `Metadata: read` installed on this repo. Required
  repo secrets: `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`,
  `CARGO_REGISTRY_TOKEN`.
