---
name: harn-release
description: Use this skill when asked to analyze pending Harn release work, fold dirty or untracked changes into a release, update CHANGELOG.md, bump the version, publish crates, tag, push, or prepare GitHub release notes for this repository.
---

# Harn Release Gate

Use this skill when asked to run the final Harn release workflow, including
analysis of pending local changes, repo-wide consistency updates, changelog
prep, version bumping, crates.io publication, tagging, and release-note
rendering.

## Source of truth

Always prefer the repo scripts:

```bash
./scripts/release_ship.sh --bump patch                     # full release
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...  # piecewise
```

Do not re-invent the release ritual from memory if `release_ship.sh` can do
it. Use normal git commands only for the parts that the release gate
intentionally does not automate: analyzing pending local work and producing
the "Prepare vX.Y.Z release" commit. Once that commit is in place and the
tree is clean, `release_ship.sh` handles audit, bump, tag, push, publish,
and GitHub release creation.

## Default workflow

1. Inspect the worktree first with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless the
   user scopes the release more narrowly.
2. Read enough diff context to summarize the pending work accurately.
3. Audit pending changes for correctness and test coverage. Add Rust tests or
   conformance pairs for new or changed user-visible behavior; fix bugs
   discovered during the audit instead of shipping them. Run
   `cargo test --workspace` and `cargo run --bin harn -- test conformance`
   before proceeding.
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
8. Stage and commit the release content with
   `git commit -m "Prepare vX.Y.Z release"`. Include every file that ships in
   this version, including `CHANGELOG.md` and docs updates. Do **not** touch
   `Cargo.toml` / `Cargo.lock` version strings — `release_ship.sh` produces
   the "Bump version to X.Y.Z" commit separately.
9. With the release content committed and the tree clean, run
   `./scripts/release_ship.sh --bump patch` (or `minor`/`major`). The script
   runs audit, dry-run publish, bump, commit, tag, push branch + tag,
   `cargo publish`, and GitHub release creation in that order.
10. For an all-in-one dry run that stops before any destructive action, use
    `./scripts/release_gate.sh full --bump patch --dry-run`.
11. Only fall back to the piecewise `release_gate.sh prepare` / `publish` /
    `notes` commands when `release_ship.sh` cannot do what you need
    (e.g. recovering a partial release).

## Expectations

- Report failures clearly and stop on the first failed gate.
- Summarize the resulting version, publish status, release notes, and required
  tag/release follow-up.
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
  the tag is pushed.
- `release_ship.sh` pushes the branch and tag **before** running
  `cargo publish`, so the GitHub release-binary workflow and downstream
  fetchers (e.g. `burin-code`'s `fetch-harn`) can start working in parallel
  with crates.io publication. The GitHub release body is created last so it
  reflects the final crates.io + git state.
- A real release has exactly two commits on top of the previous release:
  `Prepare vX.Y.Z release` (code + docs + `CHANGELOG.md`) followed by
  `Bump version to X.Y.Z` (Cargo.toml + Cargo.lock only). `release_ship.sh`
  creates the second commit automatically; the human/agent creates the
  first.
- `scripts/release_ship.sh` assumes the real release content, including docs
  consistency updates, has already been committed and the tree is clean
  before it starts.
- `verify_release_metadata.py` (run from `release_gate.sh audit`) accepts the
  pre-bump state: it passes when the top `CHANGELOG.md` entry is exactly one
  patch/minor/major step ahead of `Cargo.toml`. Running audit on a "Prepare
  vX.Y.Z release" commit is fine even though Cargo.toml still points at the
  previous version.
