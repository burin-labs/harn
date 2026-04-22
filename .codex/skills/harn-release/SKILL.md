---
name: harn-release
description: Use this skill for Harn release prep, bump PRs, publishing, tagging, and release notes.
---

# Harn Release Gate

Use this skill when asked to run the final Harn release workflow, including
analysis of pending local changes, repo-wide consistency updates, changelog
prep, version-bump PR creation, crates.io publication, tagging, and
release-note rendering.

## Source of truth

Always prefer the repo scripts:

```bash
./scripts/release_ship.sh --bump patch                     # open bump PR
./scripts/release_ship.sh --finalize                       # tag/publish after merge
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...  # piecewise
```

Do not re-invent the release ritual from memory if `release_ship.sh` can do
it. Use normal git commands only for the parts that the release gate
intentionally does not automate: analyzing pending local work and producing
the "Prepare vX.Y.Z release" PR. Once that PR lands through the merge queue,
`release_ship.sh --bump patch` handles audit, dry-run publish, bump-branch
creation, version-bump commit, branch push, and PR creation. After the bump PR
lands through the merge queue, the `Finalize Release` GitHub Action
(`.github/workflows/finalize-release.yml`) runs `release_ship.sh --finalize`
automatically, which tags `main`, publishes crates, renders notes, and
creates/updates the GitHub release. Local `release_ship.sh --finalize` remains
the recovery path when the workflow fails.

## Merge-queue mode

Direct pushes to `main` are not part of the release flow. The release has two
merge-queue-reviewed PRs:

1. `Prepare vX.Y.Z release` — code + docs + `CHANGELOG.md`, no Cargo version
   bump.
2. `Bump version to X.Y.Z` — `Cargo.toml` + `Cargo.lock` only, opened by
   `./scripts/release_ship.sh --bump patch`.

Once the bump PR lands, the `Finalize Release` GitHub Action runs
`./scripts/release_ship.sh --finalize` against updated `main` automatically.
It fires on `push` to `main` when the head commit starts with
the prefix `Bump version to` (which is exactly what `release_ship.sh --bump patch`
writes), and also exposes `workflow_dispatch` for manual re-runs. Finalize
creates/pushes the tag before `cargo publish` so release-binary workflows
and downstream fetchers can still start in parallel with crates.io
publication. Only run `--finalize` locally when the workflow fails and a
human has to recover — the script is idempotent once the tag exists at HEAD.

Both script phases run the release audit and stop on the first failed gate.
Check the exit code when the bump script returns locally; for the finalize
workflow, check the job status on the `Finalize Release` Actions page.
If non-zero, the step names in stdout tell you which gate tripped.

Typical wall-clock: ~6–10 min cold, ~2–4 min warm (sccache hot). Do not
babysit it. Start the command and work on other things. Failure modes,
roughly in frequency order:

- `release_gate.sh audit` clippy/test failure → fix the code, re-commit
  into the same "Prepare vX.Y.Z release" commit (amend), re-run.
- `publish --dry-run` failure → usually a missing `include = [...]` file
  in a crate manifest; fix and re-commit.
- bump PR creation failure → the release bump branch has usually already been
  pushed; create the PR manually from `release/vX.Y.Z` into `main`.
- `cargo publish` rate-limit / transient network during finalize → re-run
  the `Finalize Release` workflow via
  `gh workflow run finalize-release.yml --ref main` (or run
  `./scripts/release_ship.sh --finalize` locally); both are idempotent once
  the tag exists at HEAD.
- `gh release create` failure during finalize → the release is already on
  crates.io and tagged; finish manually with
  `gh release create <tag> --title <tag> --notes-file <rendered>`.

## Batching multiple tickets in one release

When several unrelated tickets are ready to ship together:

1. Merge each ticket's PR to `main` through the merge queue (each may carry a
   one-line CHANGELOG addition under an "Unreleased" heading — that is
   fine).
2. Immediately before releasing, consolidate the CHANGELOG: promote the
   "Unreleased" section to `## [vX.Y.Z] - YYYY-MM-DD` with the grouped
   entries and reference each closed ticket by number.
3. Open and land a "Prepare vX.Y.Z release" PR with that consolidation
   (docs/spec edits ride along if needed).
4. From updated `main`, run `release_ship.sh --bump patch` once to open the
   version-bump PR. The release covers all the batched tickets.
5. After the bump PR lands, run `release_ship.sh --finalize`.

Prefer larger batches over many small releases when the tickets are
topically related (e.g. the "iteration unblocker" pair, the "evidence
stack" pair). A single batched release is one cargo publish cycle instead
of N, and downstream consumers pick up a coherent surface.

## Cross-repo iteration does not wait on releases

Downstream repos (notably `burin-code`) can consume in-progress Harn
changes without a release via `./scripts/fetch-harn.sh --local` in the
consumer repo — it builds Harn from `~/projects/harn` in release mode and
installs the binaries directly. Release batching exists to control the
*published* version surface; it never blocks cross-repo iteration.

## Default workflow

1. Inspect the worktree first with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless the
   user scopes the release more narrowly.
2. Read enough diff context to summarize the pending work accurately.
3. Audit pending changes for correctness and test coverage. Add Rust tests or
   conformance pairs for new or changed user-visible behavior; fix bugs
   discovered during the audit instead of shipping them.
   - Run targeted crate tests during the inner loop (`cargo nextest run -p harn-vm`
     or `cargo test -p harn-vm`) so iteration stays fast.
   - Run `make test` and `cargo run --bin harn -- test conformance` before
     proceeding with the release mechanics. `make test` uses `cargo-nextest`
     when available and falls back to `cargo test --workspace`.
4. Do a repo-consistency sweep before shipping. Update release-facing docs and
   operator guidance as needed, especially `README.md`, `CLAUDE.md`,
   `docs/src/`, `spec/HARN_SPEC.md`, `CHANGELOG.md`, and any developer setup
   surfaces such as `scripts/dev_setup.sh`, `Makefile`, `.githooks/`, and the
   first-party `harn portal` docs.
5. If syntax, parser, lexer, or tree-sitter changed, update
   `spec/HARN_SPEC.md` first. Treat it as the formal language-spec source of
   truth.
6. Update `CHANGELOG.md` before bumping the version. The new top entry must
   describe the actual pending code changes that will ship.
7. Run `cargo fmt --all` once so the upcoming release content commit is
   formatting-clean. `release_gate.sh audit` runs `cargo fmt -- --check` and
   will reject drift later; catching it here avoids re-doing commits.
8. Stage, commit, push, and open a "Prepare vX.Y.Z release" PR. Include every
   file that ships in this version, including `CHANGELOG.md` and docs updates.
   Do **not** touch `Cargo.toml` / `Cargo.lock` version strings — the bump PR
   is separate.
9. After the release-content PR lands through the merge queue, sync `main` and
   run `./scripts/release_ship.sh --bump patch` (or `minor`/`major`). The
   script runs audit, dry-run publish, creates `release/vX.Y.Z`, commits the
   Cargo version bump, pushes that branch, and opens the bump PR.
10. After the bump PR lands through the merge queue, the `Finalize Release`
    GitHub Action runs `./scripts/release_ship.sh --finalize` automatically
    against updated `main`. The workflow runs audit, dry-run publish,
    creates/pushes the tag, publishes to crates.io, renders notes, and
    creates/updates the GitHub release. Watch the run on the Actions page;
    only run `--finalize` locally as a recovery step if the workflow
    fails (it is idempotent once the tag exists at HEAD).
11. For an all-in-one dry run that stops before any destructive action, use
    `./scripts/release_gate.sh full --bump patch --dry-run`.
12. Only fall back to the piecewise `release_gate.sh prepare` / `publish` /
    `notes` commands when `release_ship.sh` cannot do what you need
    (e.g. recovering a partial release).

## Expectations

- Report failures clearly and stop on the first failed gate.
- Summarize the resulting version, bump PR or publish status, release notes,
  and required tag/release follow-up.
- If `mdbook` is not installed, mention that the docs audit skipped mdBook build.
- If the tree is dirty, do not work around it silently for `prepare`; either
  stop or commit the intended release content first.
- If untracked files exist, call them out explicitly and decide whether they
  belong in the release before staging them.
- Treat repo consistency as part of the release gate, not an optional cleanup
  pass. If behavior changes, update human-facing docs in the same release when
  they describe that behavior.
- If local development setup changed, keep `README.md`, `CONTRIBUTING.md`,
  `scripts/dev_setup.sh`, and `Makefile` aligned so the bootstrap path stays
  obvious and low-friction.
- If observability surfaces changed, update `docs/src/portal.md` and any CLI
  references that describe `harn portal`.
- If grammar-related files changed, mention whether `spec/HARN_SPEC.md` was
  updated in the same batch.
- The grammar/spec audit now includes `scripts/verify_language_spec.py`, which
  extracts `harn` fences from `spec/HARN_SPEC.md` and runs `harn check` on
  them. Treat failures there as spec drift, not just docs drift.
- The grammar/spec audit also includes `scripts/verify_tree_sitter_parse.py`,
  which sweeps positive `.harn` programs through the executable tree-sitter
  grammar. Treat failures there as parser/grammar divergence.

## Notes

- `scripts/publish.sh` remains the crates.io publisher.
- `CHANGELOG.md` is the release-language source of truth. Use
  `scripts/render_release_notes.py` or `./scripts/release_gate.sh notes` to
  produce the exact GitHub release body from it.
- GitHub release artifacts are produced by the existing release workflow once
  the tag is pushed during finalize.
- `release_ship.sh --finalize` pushes the tag **before** running
  `cargo publish`, so the GitHub release-binary workflow and downstream
  fetchers (e.g. `burin-code`'s `fetch-harn`) can start working in parallel
  with crates.io publication. The GitHub release body is created last so it
  reflects the final crates.io + git state.
- The `Finalize Release` workflow
  (`.github/workflows/finalize-release.yml`) runs `--finalize` inside
  GitHub Actions whenever a `Bump version to X.Y.Z` commit lands on
  `main`. It authenticates to crates.io with repo secret
  `CARGO_REGISTRY_TOKEN` and to GitHub with the default `GITHUB_TOKEN`.
  `workflow_dispatch` is the manual re-trigger. Running `--finalize`
  locally is still supported for recovery but is not the default
  post-bump step.
- A real release has exactly two release commits on `main`, both landed via
  PR/merge queue: `Prepare vX.Y.Z release` (code + docs + `CHANGELOG.md`)
  followed by `Bump version to X.Y.Z` (Cargo.toml + Cargo.lock only).
  `release_ship.sh --bump patch` creates and opens the second PR
  automatically; the human/agent creates the first.
- `scripts/release_ship.sh --bump patch` assumes the real release content,
  including docs consistency updates, has already landed through the merge
  queue and the local `main` tree is clean before it starts. `--finalize`
  assumes the automated bump PR has landed through the merge queue.
- `verify_release_metadata.py` (run from `release_gate.sh audit`) accepts the
  pre-bump state: it passes when the top `CHANGELOG.md` entry is exactly one
  patch/minor/major step ahead of `Cargo.toml`. Running audit on a "Prepare
  vX.Y.Z release" commit is fine even though Cargo.toml still points at the
  previous version.
- `release_gate.sh audit` starts with a serial
  `cargo build --workspace --all-targets` warm prebuild before spawning the
  five parallel lanes. Cold wall-clock is typically ~6-10 min (dominated by
  the prebuild); a warm-cache audit finishes in ~10-30 s. Any lane that
  exceeds ~5 min after the prebuild is a real regression, not cold-cache
  noise.
