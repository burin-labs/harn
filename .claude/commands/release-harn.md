Run the full Harn release workflow from the repo source of truth.

## TL;DR

```bash
# From a clean checkout, with release content + CHANGELOG entry already in HEAD:
./scripts/release_ship.sh --bump patch
```

`release_ship.sh` handles audit, dry-run publish, bump, tag, push, crates.io
publish, and GitHub release creation in the right order. Run it once the
release content commit is in place; do not re-invent the sequence manually
unless the script cannot do what you need.

## Default workflow

1. Inspect the worktree with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless the
   user scopes the release more narrowly.
2. Read enough diff context to summarize the pending work accurately.
3. Audit all pending changes for code quality, correctness, and test coverage.
   For each changed module or feature, check whether existing Rust tests
   and conformance tests (`conformance/tests/`) adequately cover the new or
   changed behavior. Fill gaps:
   - Add or update `#[test]` functions for new/changed Rust logic.
   - Add or update `.harn` + `.expected` conformance test pairs for any
     user-visible behavior changes or new builtins/features.
   - Fix implementation bugs, edge cases, or incomplete code paths discovered
     during the audit.
   - **Targeted tests first**: run tests only for changed crates during the
     audit loop (e.g. `cargo nextest run -p harn-vm` or
     `cargo test -p harn-vm`). This keeps the edit-test cycle fast.
   - **Full gate last**: once the audit is complete and all targeted tests
     pass, run `make test-fast` (nextest, or `cargo test --workspace` as
     fallback) and `cargo run --bin harn -- test conformance` as the final
     gate before committing.
   Do not skip this step — shipping untested or buggy code is worse than
   delaying a release.
4. Do a repo-consistency sweep before shipping. Update release-facing docs and
   operator guidance as needed, especially `README.md`, `CLAUDE.md`,
   `docs/src/`, `spec/HARN_SPEC.md`, `CHANGELOG.md`, and any developer setup
   surfaces such as `scripts/dev_setup.sh`, `Makefile`, `.githooks/`,
   `CONTRIBUTING.md`, and `docs/src/portal.md`.
5. If syntax, parser, lexer, or tree-sitter changed, update
   `spec/HARN_SPEC.md` first. Treat it as the formal language-spec source of
   truth.
6. Update `CHANGELOG.md` before bumping the version. The new top entry must
   describe the actual pending code changes that will ship.
7. Run `cargo fmt --all` once so the upcoming release content commit is
   formatting-clean. `release_gate.sh audit` runs `cargo fmt -- --check` and
   will reject drift later; catching it here avoids re-doing commits.
8. Stage and commit the release content:
   `git commit -m "Prepare vX.Y.Z release"`. Include every file that ships in
   this version, including `CHANGELOG.md` and doc updates. Do **not** include
   `Cargo.toml` / `Cargo.lock` version bumps in this commit — the ship script
   produces those in a separate "Bump version to X.Y.Z" commit.
9. With the release content committed and the worktree clean, run
   `./scripts/release_ship.sh --bump patch` (or `minor`/`major`). The script
   audits, dry-run publishes, bumps `Cargo.toml`, commits the bump, tags,
   pushes branch + tag, publishes to crates.io, and creates the GitHub
   release in that order.
10. For an all-in-one dry run that stops before any destructive action,
    use `./scripts/release_gate.sh full --bump patch --dry-run`.
11. Only drop down to the piecewise `release_gate.sh prepare` / `publish` /
    `notes` commands when `release_ship.sh` cannot do what you need
    (e.g. recovering a partial release).

## Source of truth

Always prefer the repo scripts:

```bash
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
./scripts/release_ship.sh --bump patch
```

Do not re-invent the release ritual from memory if the script can do it.
Use normal git commands for the parts that the release gate intentionally does
not automate, such as analyzing pending local work, making release commits, and
pushing `main` and tags. Once the release content is committed and the tree is
clean, prefer `./scripts/release_ship.sh` for the deterministic release
mechanics.

## Rules

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
- The grammar/spec audit includes `scripts/verify_language_spec.py`, which
  extracts `harn` fences from `spec/HARN_SPEC.md` and runs `harn check` on
  them. Treat failures there as spec drift, not just docs drift.
- The grammar/spec audit also includes `scripts/verify_tree_sitter_parse.py`,
  which sweeps positive `.harn` programs through the executable tree-sitter
  grammar. Treat failures there as parser/grammar divergence.
- A real release has exactly two commits on top of the previous release:
  `Prepare vX.Y.Z release` (code + docs + `CHANGELOG.md`) followed by
  `Bump version to X.Y.Z` (Cargo.toml + Cargo.lock only). `release_ship.sh`
  creates the second commit automatically; the human/agent creates the
  first.
- `scripts/release_ship.sh` assumes the real release content, including docs
  consistency updates, has already been committed and the tree is clean before
  it starts.
- `verify_release_metadata.py` accepts the pre-bump state — it passes when
  the top `CHANGELOG.md` entry is exactly one patch/minor/major step ahead
  of `Cargo.toml`. That means running `release_ship.sh` on a "Prepare
  vX.Y.Z release" commit is fine even though Cargo.toml still points at the
  previous version.
- `release_ship.sh` pushes the branch and tag **before** calling
  `cargo publish`, so GitHub release binary workflows and downstream
  fetchers (e.g. `burin-code`'s `fetch-harn`) start working in parallel
  with crates.io publication. The GitHub release body is created last so
  it can reference the final crates.io state.
- `CHANGELOG.md` is the release-language source of truth. Use
  `scripts/render_release_notes.py` or `./scripts/release_gate.sh notes` to
  produce the exact GitHub release body from it.

## Useful shortcuts

```bash
./scripts/release_ship.sh --bump patch
./scripts/release_gate.sh full --bump patch --dry-run
./scripts/release_gate.sh notes
```
