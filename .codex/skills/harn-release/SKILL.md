# Harn Release Gate

Use this skill when asked to run the final Harn docs audit, verification gate,
version bump, tagging, release notes prep, or crates publish flow.

## Source of truth

Always prefer the repo command:

```bash
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
```

Do not re-invent the release ritual from memory if the script can do it.

## Default workflow

1. If syntax, parser, lexer, or tree-sitter changed, update
   `spec/HARN_SPEC.md` first. Treat it as the formal language-spec source of
   truth.
2. Run `./scripts/release_gate.sh audit`.
3. If the user wants a version bump, run
   `./scripts/release_gate.sh prepare --bump patch` unless they specify
   `minor` or `major`.
4. If the user wants publication, run
   `./scripts/release_gate.sh publish --dry-run` first unless they explicitly
   ask for real publish immediately.
5. Before tagging or creating the GitHub release, render the changelog-backed
   release body with `./scripts/release_gate.sh notes`.
6. For an all-in-one dry run, use:

```bash
./scripts/release_gate.sh full --bump patch --dry-run
```

## Expectations

- Report failures clearly and stop on the first failed gate.
- Summarize the resulting version, publish status, and required tag/release follow-up.
- If `mdbook` is not installed, mention that the docs audit skipped mdBook build.
- If the tree is dirty, do not work around it silently for prepare/publish.
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
- GitHub release artifacts are produced by the existing release workflow once the tag is pushed.
