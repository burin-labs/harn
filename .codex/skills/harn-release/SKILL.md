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
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
./scripts/release_ship.sh --bump patch
```

Do not re-invent the release ritual from memory if the script can do it.
Use normal git commands for the parts that the release gate intentionally does
not automate, such as analyzing pending local work, making release commits, and
pushing `main` and tags. Once the release content is committed and the tree is
clean, prefer `./scripts/release_ship.sh` for the deterministic release
mechanics.

## Default workflow

1. Inspect the worktree first with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless the
   user scopes the release more narrowly.
2. Read enough diff context to summarize the pending work accurately.
3. Do a repo-consistency sweep before shipping. Update release-facing docs and
   operator guidance as needed, especially `README.md`, `CLAUDE.md`,
   `docs/src/`, `spec/HARN_SPEC.md`, and `CHANGELOG.md`.
4. If syntax, parser, lexer, or tree-sitter changed, update
   `spec/HARN_SPEC.md` first. Treat it as the formal language-spec source of
   truth.
5. Update `CHANGELOG.md` before bumping the version. The new top entry must
   describe the actual pending code changes that will ship.
6. Run `./scripts/release_gate.sh audit`.
7. If the user wants publication, run
   `./scripts/release_gate.sh publish --dry-run` before the real publish unless
   they explicitly ask to skip the dry run.
8. If the tree is dirty and the user wants those local changes released, stage
   and commit them before `prepare`. Use `git add -A` so untracked files are
   included too.
9. After the release content commit is in place, prefer
   `./scripts/release_ship.sh --bump patch` for the mechanical release
   sequence. Change `patch` to `minor` or `major` if requested.
10. If you need to perform those steps manually instead of using
    `release_ship.sh`, run `./scripts/release_gate.sh prepare --bump ...`,
    commit the version bump, render notes with
    `./scripts/release_gate.sh notes`, run
    `./scripts/release_gate.sh publish`, then tag and push.
11. For an all-in-one dry run, use:

```bash
./scripts/release_gate.sh full --bump patch --dry-run
```

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
- Prefer two commits for a real release when local feature/fix work is still
  pending:
  `git commit -m "<describe release content>"` followed by
  `git commit -m "Bump version to X.Y.Z"`.
- `scripts/release_ship.sh` assumes the real release content, including docs
  consistency updates, has already been committed and the tree is clean before
  it starts.
