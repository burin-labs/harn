# Harn Release Command

Run the full Harn release workflow from the repo source of truth.

Default assumptions:

- Analyze the current worktree first with `git status --short`,
  `git diff --stat`, and targeted `git diff` reads.
- Include all tracked and untracked local work in the release unless the user
  scopes it differently.
- Before any release mechanics, do a repo-consistency sweep and update release-
  facing docs as needed, including `README.md`, `CLAUDE.md`, `docs/src/`,
  `spec/HARN_SPEC.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, and developer setup
  surfaces such as `scripts/dev_setup.sh`, `Makefile`, `.githooks/`, and
  `docs/src/portal.md`.
- Commit the actual release content first, including untracked files that
  belong in the release.
- Then prefer the deterministic ship script:

```bash
./scripts/release_ship.sh --bump patch
```

- Adjust `patch` to `minor` or `major` if requested.

Rules:

- `./scripts/release_ship.sh` is the default mechanical entry point once the
  release content and docs are consistent and committed.
- Prefer `./scripts/release_gate.sh <audit|prepare|publish|notes|full>` over
  ad hoc release commands when working below the ship script.
- Do not bypass a dirty tree silently for `prepare` or `release_ship.sh`;
  either stop or commit the intended release content first.
- If syntax, parser, lexer, or tree-sitter changed, update
  `spec/HARN_SPEC.md` before the final gate.
- If command behavior, release workflow, or operator guidance changed, update
  `README.md`, `CLAUDE.md`, `CONTRIBUTING.md`, and mdBook pages that describe
  the changed surface.
- Treat `CHANGELOG.md` as the source of truth for GitHub release notes.
- Summarize the shipped version, the release-content commit, the bump commit,
  publish status, and the exact notes body or compare link.

Useful shortcuts:

```bash
./scripts/release_ship.sh --bump patch
./scripts/release_gate.sh full --bump patch --dry-run
./scripts/release_gate.sh notes
```
