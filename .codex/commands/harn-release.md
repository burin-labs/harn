# Harn Release Command

Run the full Harn release workflow from the repo source of truth.

Default assumptions:

- Analyze the current worktree first with `git status --short`,
  `git diff --stat`, and targeted `git diff` reads.
- Include all tracked and untracked local work in the release unless the user
  scopes it differently.
- Before any release mechanics, do a repo-consistency sweep and update
  release-facing docs as needed, including `README.md`, `CLAUDE.md`,
  `docs/src/`, `spec/HARN_SPEC.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, and
  developer setup surfaces such as `scripts/dev_setup.sh`, `Makefile`,
  `.githooks/`, and `docs/src/portal.md`.
- Run `cargo fmt --all` once so the upcoming release content commit is
  formatting-clean — `release_gate.sh audit` runs `cargo fmt -- --check` and
  will reject drift later.
- Commit the release content with `git commit -m "Prepare vX.Y.Z release"`.
  Include every file that ships in this version (code + docs + `CHANGELOG.md`)
  but **not** `Cargo.toml` / `Cargo.lock` — `release_ship.sh` creates the
  "Bump version to X.Y.Z" commit separately.
- Then run the deterministic ship script:

```bash
./scripts/release_ship.sh --bump patch
```

- Adjust `patch` to `minor` or `major` if requested.

Rules:

- `./scripts/release_ship.sh` is the default mechanical entry point once the
  release content and docs are consistent and committed. It runs audit,
  dry-run publish, bump, commit, tag, push branch + tag, `cargo publish`,
  and GitHub release creation in that order. The push happens **before**
  `cargo publish` so GitHub release-binary workflows and downstream
  fetchers (e.g. `burin-code`'s `fetch-harn`) start in parallel with
  crates.io. The GitHub release body is created last.
- `verify_release_metadata.py` accepts the pre-bump state — it passes when
  `CHANGELOG.md` top is exactly one patch/minor/major step ahead of
  `Cargo.toml`. That is why running `release_ship.sh` on a "Prepare vX.Y.Z
  release" commit is fine even though Cargo.toml still points at the
  previous version.
- Prefer `./scripts/release_gate.sh <audit|prepare|publish|notes|full>` over
  ad hoc release commands only when working below the ship script (e.g.
  recovering from a partial release).
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
