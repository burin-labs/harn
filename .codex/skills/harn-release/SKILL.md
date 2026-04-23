---
name: harn-release
description: Use this skill for Harn release prep, bump PRs, publishing, tagging, and release notes.
---

# Harn Release Gate

The **only** part of a Harn release that requires your intuition is writing
and landing the **"Prepare vX.Y.Z release"** PR. Everything from there is
automated by GitHub Actions running under the `harn-release-bot` App
identity.

## End-state flow

```text
human/agent: write "Prepare vX.Y.Z release" PR (changelog, docs, code)
        │  ↓ lands through merge queue
bot:    Bump Release workflow opens "Bump version to X.Y.Z" PR
        │  ↓ CI green, lands through merge queue
bot:    Finalize Release workflow tags + crates.io + GH release notes
        │  ↓ tag push cascades
bot:    Release workflow builds binaries + multi-arch container
        │  ↓
        v0.7.X is shipped
```

The three bot workflows live at:

- `.github/workflows/bump-release.yml`
- `.github/workflows/finalize-release.yml`
- `.github/workflows/release.yml`

## Source of truth

All bot workflows invoke the same scripts you'd run locally for recovery:

```bash
# Recovery only — bot does these by default:
./scripts/release_ship.sh --bump <patch|minor|major>
./scripts/release_ship.sh --finalize

# Lower-level pieces, when --bump/--finalize can't help:
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
```

Do not re-invent the release ritual from memory if the script can do it.
The only step that is intentionally not scripted is producing the
"Prepare vX.Y.Z release" PR.

## Merge-queue mode

A real release has exactly two release commits on `main`, both landed via
PR/merge queue:

1. **`Prepare vX.Y.Z release`** — code + docs + `CHANGELOG.md`, no Cargo
   version bump. Written by you.
2. **`Bump version to X.Y.Z`** — `Cargo.toml` + `Cargo.lock` only,
   opened by Bump Release workflow under the bot identity.

Watch the Actions runs after #1 lands. Do not poll local status. The
finalize and release workflows fire downstream automatically.

Failure modes, roughly in frequency order, with recovery:

- `release_gate.sh audit` clippy / test failure during bump or finalize:
  fix the code in a small follow-up PR (treat it as release-prep
  cleanup), let it land, then re-trigger the failed workflow via
  `gh workflow run <name> --ref main`.
- `publish --dry-run` failure: usually a missing `include = [...]` file
  in a crate manifest. Fix in a small PR, re-trigger.
- `cargo publish` rate-limit / transient network during finalize:
  re-trigger Finalize Release; it falls back to per-crate publish and
  treats `already exists on crates.io index` as success.
- Release workflow needs to re-emit binaries for an already-tagged
  version: `gh workflow run release.yml --ref main -f tag=vX.Y.Z`.

## Batching multiple tickets in one release

When several unrelated tickets are ready to ship together:

1. Merge each ticket's PR to `main` through the merge queue (each may
   carry a one-line CHANGELOG addition under an "Unreleased" heading —
   that is fine).
2. Immediately before releasing, consolidate the CHANGELOG: promote the
   "Unreleased" section to `## vX.Y.Z` with the grouped entries and
   reference each closed ticket by number.
3. Open and land the **"Prepare vX.Y.Z release"** PR with that
   consolidation (docs / spec edits ride along if needed).
4. Walk away. The bot opens the bump PR, that lands, the bot finalizes.

Prefer larger batches over many small releases when the tickets are
topically related. A single batched release is one cargo publish cycle
instead of N, and downstream consumers pick up a coherent surface.

## Cross-repo iteration does not wait on releases

Downstream repos (notably `burin-code`) can consume in-progress Harn
changes without a release via `./scripts/fetch-harn.sh --local` in the
consumer repo — it builds Harn from `~/projects/harn` in release mode
and installs the binaries directly. Release batching exists to control
the *published* version surface; it never blocks cross-repo iteration.

## What you actually do for a release

Steps 1-8 are the only ones requiring judgment. After step 8 you are
done — do **not** run `release_ship.sh` locally as a default step.

1. Inspect the worktree first with `git status --short` and
   `git diff --stat`. Treat tracked and untracked changes as candidate
   release content unless the user scopes the release more narrowly.
2. Read enough diff context to summarize the pending work accurately.
3. Audit pending changes for correctness and test coverage. Add Rust
   tests or conformance pairs for new or changed user-visible behavior;
   fix bugs discovered during the audit instead of shipping them.
   - Targeted crate tests during the inner loop (`cargo nextest run -p
     harn-vm` or `cargo test -p harn-vm`).
   - `make test` and `cargo run --bin harn -- test conformance` before
     proceeding with release mechanics.
4. Repo-consistency sweep before shipping. Update release-facing docs
   and operator guidance as needed: `README.md`, `CLAUDE.md`,
   `docs/src/`, `spec/HARN_SPEC.md`, `CHANGELOG.md`, and developer-setup
   surfaces (`scripts/dev_setup.sh`, `Makefile`, `.githooks/`,
   `docs/src/portal.md`).
5. If syntax / parser / lexer / tree-sitter changed, update
   `spec/HARN_SPEC.md` first — formal language-spec source of truth.
6. Update `CHANGELOG.md` with a new top entry `## vX.Y.Z`. The version
   chosen here drives the bump type the bot derives. Pick deliberately.
7. Run `cargo fmt --all` once so the prep commit is formatting-clean.
8. Stage, commit, push, open the **"Prepare vX.Y.Z release"** PR.
   Include code + docs + `CHANGELOG.md` for this version. Do **not**
   include `Cargo.toml` / `Cargo.lock` version bumps.

## Expectations

- Stop on the first failed gate in the prepare PR. Do not paper over.
- Once the prepare PR lands, watch the Actions UI. The bump → finalize →
  release cascade should complete in ~20-30 min wall-clock total. Each
  workflow has `workflow_dispatch` for manual recovery.
- Treat repo consistency as part of the prepare PR, not an optional
  cleanup pass. If behavior changes, update human-facing docs in the
  same prep PR.
- The grammar / spec audit includes `scripts/verify_language_spec.py`
  (extracts ` ```harn ` fences from `spec/HARN_SPEC.md` and runs `harn
  check`) and `scripts/verify_tree_sitter_parse.py` (sweeps positive
  `.harn` programs through the executable tree-sitter grammar). Treat
  failures as spec drift, not just docs drift.

## Notes

- `scripts/publish.sh` remains the crates.io publisher. It tries
  `cargo publish --workspace` first with retries, then falls back to
  per-crate publish where `already exists on crates.io index` is
  treated as success — so partial-publish recovery is automatic.
- `CHANGELOG.md` is the release-language source of truth. Notes are
  rendered from it by `scripts/render_release_notes.py` /
  `scripts/release_gate.sh notes`.
- GitHub release artifacts (binary tarballs + GHCR container) are
  produced by `release.yml` once the tag is pushed. Tag push from
  Finalize Release uses the App identity, so the cascade fires
  normally — `GITHUB_TOKEN`-pushed tags would NOT trigger it.
- `release_ship.sh --finalize` pushes the tag **before** running
  `cargo publish`, so the GitHub release-binary workflow and downstream
  fetchers (e.g. `burin-code`'s `fetch-harn`) can start working in
  parallel with crates.io publication.
- `verify_release_metadata.py` (run from `release_gate.sh audit`)
  accepts the pre-bump state: it passes when the top `CHANGELOG.md`
  entry is exactly one patch / minor / major step ahead of `Cargo.toml`.
  The Bump Release workflow's `scripts/detect_bump_type.py` relies on
  this same invariant.
- `release_gate.sh audit` starts with a serial
  `cargo build --workspace --all-targets` warm prebuild before spawning
  the five parallel lanes. Cold ~6-10 min, warm ~10-30 s.
- The release-bot App needs `Contents: write`, `Pull requests: write`,
  `Actions: write`, `Metadata: read` installed on this repo. Required
  repo secrets: `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`,
  `CARGO_REGISTRY_TOKEN`.
