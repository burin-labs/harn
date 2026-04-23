# Harn Release Command

Run the merge-queue-safe Harn release workflow.

The **only** part of a release that requires intuition is writing and landing
the **"Prepare vX.Y.Z release"** PR. Everything from there is automated by
GitHub Actions running under the `harn-release-bot` App identity.

## End-state flow

```text
human/agent: write "Prepare vX.Y.Z release" PR (changelog, docs, code)
        │  ↓ lands through merge queue
bot:    Bump Release workflow opens "Bump version to X.Y.Z" PR
        │  ↓ CI green, lands through merge queue
bot:    Finalize Release workflow tags + crates.io + GH release notes
        │  ↓ tag push cascades
bot:    Release workflow builds binaries + multi-arch container
        │  ↓
        v0.7.X is shipped (binaries, container, crates.io, release notes)
```

## What you (the agent) actually do

The work is in the prepare PR. After that, hands off.

1. Inspect the worktree first with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless
   the user scopes it differently.
2. Read enough diff context to summarize the pending work accurately.
3. Audit pending changes for correctness and test coverage. Add Rust tests
   or conformance pairs for new or changed user-visible behavior; fix bugs
   discovered during the audit instead of shipping them.
   - Targeted crate tests in the inner loop (`cargo nextest run -p harn-vm`).
   - `make test` and `cargo run --bin harn -- test conformance` before
     proceeding with release mechanics.
4. Repo-consistency sweep. Update release-facing docs as needed:
   `README.md`, `CLAUDE.md`, `docs/src/`, `spec/HARN_SPEC.md`,
   `CHANGELOG.md`, and developer-setup surfaces (`scripts/dev_setup.sh`,
   `Makefile`, `.githooks/`, the first-party `harn portal` docs).
5. If syntax / parser / lexer / tree-sitter changed, update
   `spec/HARN_SPEC.md` first. It is the formal language-spec source of
   truth.
6. Update `CHANGELOG.md` with a new top entry `## vX.Y.Z` describing the
   actual pending code changes that will ship. The version chosen here
   (patch / minor / major bump from the current Cargo.toml) drives what
   the bot opens later — pick deliberately.
7. Run `cargo fmt --all` once so the prep commit is formatting-clean.
   `release_gate.sh audit` runs `cargo fmt -- --check` and rejects drift
   later.
8. Stage, commit, push, and open the **"Prepare vX.Y.Z release"** PR.
   Include every file that ships in this version: code, docs,
   `CHANGELOG.md`. Do **not** touch `Cargo.toml` / `Cargo.lock` version
   strings — the bot's bump PR carries those.

That's it. Stop here. The bot takes over.

## What happens automatically after the prepare PR lands

- **`Bump Release`** workflow (`.github/workflows/bump-release.yml`) fires
  on push to `main` when the head commit starts with `Prepare v`. It
  derives the bump type via `scripts/detect_bump_type.py` (compares
  `CHANGELOG.md` top entry to `Cargo.toml`), then runs
  `./scripts/release_ship.sh --bump <type>` under the App identity. That
  audits, dry-run publishes, bumps `Cargo.toml` + `Cargo.lock`, commits,
  pushes `release/vX.Y.Z`, and opens the bump PR.
- The bump PR's CI fires normally (App-pushed branches don't trigger
  GHA's downstream-workflow suppression). Auto-merge on, queue lands it.
- **`Finalize Release`** workflow
  (`.github/workflows/finalize-release.yml`) fires on push to `main` when
  the head commit starts with `Bump version to`. It runs
  `./scripts/release_ship.sh --finalize` under the App identity: audit,
  dry-run publish, push the tag, `cargo publish`, render notes, create
  or update the GitHub release.
- The tag push triggers **`Release`** workflow
  (`.github/workflows/release.yml`), which builds the darwin/linux ×
  x86/arm binary tarballs, publishes the multi-arch GHCR container, and
  attaches the binaries to the GitHub release.

## Recovery (only when something breaks)

- **A workflow failed mid-run**: re-trigger from the GitHub Actions UI
  (each workflow exposes `workflow_dispatch`). The scripts are
  idempotent — per-crate publish skips already-published, the tag step
  no-ops if it already points where it should, `gh release` is
  view-then-edit-or-create.
- **The Release workflow needs to re-emit binaries** for an
  already-tagged version: `gh workflow run release.yml --ref main -f
  tag=vX.Y.Z`. The workflow accepts the tag input and runs at that
  tagged code.
- **Truly stuck local recovery** (very rare): run
  `./scripts/release_ship.sh --bump patch` or `--finalize` from updated
  `main`. Same script the bot runs.

## Rules

- `release_ship.sh --bump` and `--finalize` are now bot-driven by
  default. Running them locally is a recovery action, not the default
  step. Never amend or skip the bot path silently.
- A real release has exactly two release commits on `main`, both landed
  via PR/merge queue: `Prepare vX.Y.Z release` (you) followed by
  `Bump version to X.Y.Z` (bot).
- Do not bypass a dirty tree silently for `prepare`; either stop or
  commit the intended release content first.
- If syntax / parser / lexer / tree-sitter changed, update
  `spec/HARN_SPEC.md` before opening the prepare PR.
- If command behavior, release workflow, or operator guidance changed,
  update `README.md`, `CLAUDE.md`, `CONTRIBUTING.md`, and mdBook pages
  that describe the changed surface in the same prep PR.
- Treat `CHANGELOG.md` as the source of truth for GitHub release notes.
  `scripts/render_release_notes.py` and `release_gate.sh notes` derive
  the body from it.
- `release_gate.sh audit` begins with a serial `cargo build --workspace
  --all-targets` warm prebuild before spawning the five parallel lanes.
  Cold wall-clock ~6-10 min, warm ~10-30 s.
- The release-bot App needs `Contents: write`, `Pull requests: write`,
  `Actions: write`, `Metadata: read` installed on this repo. Repo
  secrets that gate the workflows: `RELEASE_APP_ID`,
  `RELEASE_APP_PRIVATE_KEY`, `CARGO_REGISTRY_TOKEN`.

## Useful shortcuts

```bash
# All-in-one dry run, stops before any destructive action:
./scripts/release_gate.sh full --bump patch --dry-run

# Render the GitHub release body locally from CHANGELOG.md:
./scripts/release_gate.sh notes

# Manually re-trigger workflows:
gh workflow run bump-release.yml --ref main
gh workflow run finalize-release.yml --ref main
gh workflow run release.yml --ref main -f tag=vX.Y.Z
```
