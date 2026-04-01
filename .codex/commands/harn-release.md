# Harn Release Command

Run the Harn release gate from the repo source of truth:

```bash
./scripts/release_gate.sh full --bump patch --dry-run
```

Adjust:

- `patch` to `minor` or `major` if requested.
- `full` to `audit`, `prepare`, or `publish` for a narrower pass.
- Use `./scripts/release_gate.sh notes` to render the changelog-backed GitHub
  release body locally before creating or updating a release.
- Remove `--dry-run` only when the user explicitly wants real crates.io publication.
- If syntax, parser, lexer, or tree-sitter changed, update `spec/HARN_SPEC.md`
  before running the final gate.
