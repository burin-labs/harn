# Harn Release Command

Run the merge-queue-safe Harn release workflow.

The release is **one** human PR titled `Release vX.Y.Z`. It carries the
changelog, code, docs, AND the `Cargo.toml`/`Cargo.lock` bump together. After
it lands through the merge queue, the Publish release workflow auto-fires on
tag drift and ships everything. No second PR.

## End-state flow

```text
human/agent: write & land "Release vX.Y.Z" PR (changelog + code + docs + bump)
        │  ↓ merge queue runs full audit set in CI
bot:    Publish release workflow auto-fires on tag drift
        │   pushes vX.Y.Z, runs cargo publish, creates GH release notes
        │  ↓ tag push cascades
bot:    Release workflow builds binaries + multi-arch container
        │  ↓
        v0.7.X is shipped (binaries, container, crates.io, release notes)
```

## What you (the agent) actually do

The work is in the one release PR. After that, hands off.

1. Branch off main: `git checkout -b release/vX.Y.Z`. Conventional name; the
   workflow keys on tag drift, not branch name.
2. Inspect the worktree first with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless
   the user scopes it differently.
3. Read enough diff context to summarize the pending work accurately.
4. Audit pending changes for correctness and test coverage. Add Rust tests
   or conformance pairs for new or changed user-visible behavior; fix bugs
   discovered during the audit instead of shipping them.
   - Targeted crate tests in the inner loop (`cargo nextest run -p harn-vm`).
   - `make test` and `cargo run --bin harn -- test conformance` before
     proceeding with release mechanics.
5. Repo-consistency sweep. Update release-facing docs as needed:
   `README.md`, `CLAUDE.md`, `docs/src/`, `spec/HARN_SPEC.md`,
   `CHANGELOG.md`, and developer-setup surfaces (`scripts/dev_setup.sh`,
   `Makefile`, `.githooks/`, the first-party `harn portal` docs).
6. If syntax / parser / lexer / tree-sitter changed, update
   `spec/HARN_SPEC.md` first. It is the formal language-spec source of
   truth. The pre-commit hook regenerates `docs/src/language-spec.md`
   automatically; CI gates on it via `make check-language-spec`.
7. Update `CHANGELOG.md` with a new top entry `## vX.Y.Z` describing the
   actual pending code changes that will ship. The version chosen here
   (patch / minor / major from the current `Cargo.toml`) drives what
   `--prepare` will bump to next — pick deliberately.
8. Run the consolidated prep:

   ```bash
   ./scripts/release_ship.sh --prepare --bump patch
   ```

   This audits, dry-run-publishes, bumps `Cargo.toml`/`Cargo.lock`/per-crate
   manifests, regenerates derived files, and `git add`s everything. Use
   `--skip-audit` / `--skip-dry-run` to trust the merge-queue CI when
   iterating.

9. Commit, push, open the PR titled **`Release vX.Y.Z`**:

   ```bash
   git commit -m "Release vX.Y.Z"
   git push -u origin release/vX.Y.Z
   gh pr create --title "Release vX.Y.Z" --body "..."
   ```

That's it. Stop here. The bot takes over once the PR lands.

## What happens automatically after the release PR lands

- **`Publish release`** workflow
  (`.github/workflows/publish-release.yml`) detects tag drift
  (`Cargo.toml` ahead of latest `vX.Y.Z` tag) and runs
  `./scripts/release_ship.sh --finalize` under the App identity:
  portal-check + publish dry-run + push tag + `cargo publish` + render
  notes + create or update the GitHub release. **Audit is skipped** —
  the merge-queue CI of the just-landed Release PR proved the same
  gates a few minutes ago.
- The tag push triggers **`Build release binaries`** workflow
  (`.github/workflows/build-release-binaries.yml`), which builds the darwin/linux ×
  x86/arm binary tarballs, publishes the multi-arch GHCR container,
  and attaches the binaries to the GitHub release.

## Recovery (only when something breaks)

- **Finalize failed mid-run**: re-trigger from the GitHub Actions UI
  (`gh workflow run publish-release.yml`). Scripts are idempotent —
  per-crate publish skips already-published, the tag step no-ops if
  it already points where it should, `gh release` is
  view-then-edit-or-create. Pass `reaudit: true` if you want it to
  re-run the full audit (only needed if main has changed since the
  PR landed).
- **Build release binaries workflow needs to re-emit binaries** for an
  already-tagged version: `gh workflow run release.yml --ref main -f
  tag=vX.Y.Z`. The workflow accepts the tag input and runs at that
  tagged code.
- **Accidentally landed a "Prepare vX.Y.Z release"-style commit on
  main without the consolidated bump**: the `Open version bump PR (recovery)`
  workflow exists for this. `gh workflow run bump-release.yml` opens
  a historical-style bump PR.
- **Truly stuck local recovery (very rare)**: run
  `./scripts/release_ship.sh --prepare --bump patch` from a fresh
  release branch, or `./scripts/release_ship.sh --finalize` from
  updated `main`. Same scripts the bot runs.

## Rules

- The default release flow is **one PR titled `Release vX.Y.Z`**. The
  legacy two-PR flow (`Prepare vX.Y.Z release` → bot opens `Bump
  version to X.Y.Z`) is recovery only.
- `release_ship.sh --finalize` is bot-driven by default. Running it
  locally is a recovery action.
- Do not amend or skip the bot finalize path silently.
- If syntax / parser / lexer / tree-sitter changed, update
  `spec/HARN_SPEC.md` before running `--prepare`. The pre-commit
  hook regenerates `docs/src/language-spec.md` for you.
- If command behavior, release workflow, or operator guidance changed,
  update `README.md`, `CLAUDE.md`, `CONTRIBUTING.md`, and mdBook pages
  that describe the changed surface in the same release PR.
- Treat `CHANGELOG.md` as the source of truth for GitHub release notes.
  `scripts/render_release_notes.py` and `release_gate.sh notes` derive
  the body from it. `verify_release_metadata.py` (now in merge-queue
  CI) rejects malformed headings, duplicates, out-of-order entries,
  and empty section bodies.
- `release_gate.sh audit` (called by `--prepare`) starts with a
  serial `cargo build --workspace --all-targets` warm prebuild before
  spawning the five parallel lanes. Cold wall-clock ~6-10 min, warm
  ~10-30 s. Finalize no longer pays this cost.
- The release-bot App needs `Contents: write`, `Pull requests: write`,
  `Actions: write`, `Metadata: read` installed on this repo. Repo
  secrets that gate the workflows: `RELEASE_APP_ID`,
  `RELEASE_APP_PRIVATE_KEY`, `CARGO_REGISTRY_TOKEN`.

## Useful shortcuts

```bash
# Consolidated prep (the default release entry point):
./scripts/release_ship.sh --prepare --bump patch

# All-in-one dry run, stops before any destructive action:
./scripts/release_gate.sh full --bump patch --dry-run

# Render the GitHub release body locally from CHANGELOG.md:
./scripts/release_gate.sh notes

# Manually re-trigger workflows (recovery):
gh workflow run publish-release.yml --ref main
gh workflow run publish-release.yml --ref main -f reaudit=true
gh workflow run build-release-binaries.yml --ref main -f tag=vX.Y.Z
gh workflow run bump-release.yml          # legacy two-PR recovery
```
