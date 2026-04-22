# Harn Release Command

Run the merge-queue-safe Harn release workflow from the repo source of truth.

Default assumptions:

- Analyze the current worktree first with `git status --short`,
  `git diff --stat`, and targeted `git diff` reads.
- Use targeted crate tests during the audit loop, then run `make test` plus
  `cargo run --bin harn -- test conformance` as the final Rust/conformance
  gate before release. `make test` uses `cargo-nextest` when available and
  falls back to `cargo test --workspace`.
- Include all tracked and untracked local work in the release unless the user
  scopes it differently.
- Before any release mechanics, do a repo-consistency sweep and update
  release-facing docs as needed, including `README.md`, `CLAUDE.md`,
  `docs/src/`, `spec/HARN_SPEC.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, and
  developer setup surfaces such as `scripts/dev_setup.sh`, `Makefile`,
  `.githooks/`, and `docs/src/portal.md`.
- Run `cargo fmt --all` once so the upcoming release content PR is
  formatting-clean — `release_gate.sh audit` runs `cargo fmt -- --check` and
  will reject drift later.
- Open and land a "Prepare vX.Y.Z release" PR. Include every file that ships
  in this version (code + docs + `CHANGELOG.md`) but **not** `Cargo.toml` /
  `Cargo.lock` — `release_ship.sh` creates the "Bump version to X.Y.Z" PR
  separately.
- After that PR lands through the merge queue, sync `main` and run:

```bash
./scripts/release_ship.sh --bump patch
```

- Adjust `patch` to `minor` or `major` if requested. This opens the automated
  version-bump PR.
- After the version-bump PR lands through the merge queue, sync `main` and
  finalize:

```bash
./scripts/release_ship.sh --finalize
```

Rules:

- `./scripts/release_ship.sh --bump patch` is the default mechanical entry
  point once the release content and docs are consistent and landed on `main`.
  It runs audit, dry-run publish, bump, commit, pushes `release/vX.Y.Z`, and
  opens the version-bump PR. It does not tag, publish, or push to `main`.
- `./scripts/release_ship.sh --finalize` runs only after the bump PR lands on
  `main`. It runs audit, dry-run publish, creates/pushes the tag, publishes to
  crates.io, and creates/updates the GitHub release. The tag push happens
  **before** `cargo publish` so GitHub release-binary workflows and downstream
  fetchers (e.g. `burin-code`'s `fetch-harn`) start in parallel with crates.io.
  The GitHub release body is created last.
- `verify_release_metadata.py` accepts the pre-bump state — it passes when
  `CHANGELOG.md` top is exactly one patch/minor/major step ahead of
  `Cargo.toml`. That is why running `release_ship.sh --bump patch` on a
  "Prepare vX.Y.Z release" commit is fine even though Cargo.toml still points
  at the previous version.
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
- Summarize the shipped version, the release-content PR, the bump PR/commit,
  publish status, and the exact notes body or compare link.
- `release_gate.sh audit` begins with a serial `cargo build --workspace
  --all-targets` warm prebuild before spawning the five parallel lanes, so
  the lanes don't serialize on the cargo lock. Expect ~6-10 min cold and
  ~10-30 s with a warm target dir; lanes that exceed ~5 min after the
  prebuild are real regressions.

Useful shortcuts:

```bash
./scripts/release_ship.sh --bump patch
./scripts/release_ship.sh --finalize
./scripts/release_gate.sh full --bump patch --dry-run
./scripts/release_gate.sh notes
```
