Run the merge-queue-safe Harn release workflow.

## TL;DR

The release is **one** human PR titled `Release vX.Y.Z`. It carries the
changelog, code, docs, AND the `Cargo.toml`/`Cargo.lock` version bump together.
After it lands through the merge queue, the Publish release workflow
auto-fires on tag drift, ships to crates.io, tags `vX.Y.Z`, and triggers the
binary/container build. No second PR.

```text
human/agent: write & land "Release vX.Y.Z" PR
        │
        ▼  PR lands through merge queue (full audit ran in CI)
bot:    Publish release workflow auto-fires on tag drift
        │   pushes vX.Y.Z, runs cargo publish, creates GH release
        ▼  tag push cascades
bot:    Release workflow builds binaries + multi-arch container
        │
        ▼
        v0.7.X is shipped (binaries, container, crates.io, release notes)
```

## What the human/agent owns

Steps 1-9 are the only steps that need judgment. After step 9 you are done
— do **not** run `release_ship.sh --finalize` locally as a default step.

1. Branch off main: `git checkout -b release/vX.Y.Z`. The branch name is
   conventional; the workflow keys on tag drift, not branch name.
2. Inspect the worktree with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless the
   user scopes the release more narrowly.
3. Read enough diff context to summarize the pending work accurately.
4. Audit all pending changes for code quality, correctness, and test
   coverage. For each changed module or feature, check whether existing Rust
   tests and conformance tests (`conformance/tests/`) cover the new or
   changed behavior. Fill gaps:
   - Add or update `#[test]` functions for new/changed Rust logic.
   - Add or update `.harn` + `.expected` conformance test pairs for any
     user-visible behavior changes or new builtins/features.
   - Fix implementation bugs, edge cases, or incomplete code paths
     discovered during the audit.
   - **Targeted tests first**: run tests only for changed crates during the
     audit loop (e.g. `cargo nextest run -p harn-vm`). Keeps the
     edit-test cycle fast.
   - **Full gate last**: once the audit is complete and all targeted tests
     pass, run `make test` and `cargo run --bin harn -- test conformance` as
     the final gate before continuing.
   Do not skip this step — shipping untested or buggy code is worse than
   delaying a release.
5. Repo-consistency sweep. Update release-facing docs and operator guidance
   as needed: `README.md`, `CLAUDE.md`, `docs/src/`, `spec/HARN_SPEC.md`,
   `CHANGELOG.md`, and developer-setup surfaces (`scripts/dev_setup.sh`,
   `Makefile`, `.githooks/`, `CONTRIBUTING.md`, `docs/src/portal.md`).
6. If syntax, parser, lexer, or tree-sitter changed, update
   `spec/HARN_SPEC.md` first — it is the formal language-spec source of
   truth. The pre-commit hook regenerates `docs/src/language-spec.md`
   automatically; CI gates on it via `make check-language-spec`.
7. Update `CHANGELOG.md` with a new top entry `## vX.Y.Z` describing the
   actual pending code changes that will ship. The version chosen here
   (patch / minor / major bump from the current `Cargo.toml`) drives what
   `--prepare` will bump to in the next step — pick deliberately.
8. Run the consolidated prep script:

   ```bash
   ./scripts/release_ship.sh --prepare --bump patch
   ```

   This audits, dry-run-publishes, bumps `Cargo.toml`/`Cargo.lock`/per-crate
   manifests, regenerates derived files (highlight keywords, language-spec
   mirror), and `git add`s everything. Use `--skip-audit` to trust the
   merge-queue CI when iterating fast; use `--skip-dry-run` for the same
   reason on the publish dry-run.

9. Commit, push, open the PR titled **`Release vX.Y.Z`**:

   ```bash
   git commit -m "Release vX.Y.Z"
   git push -u origin release/vX.Y.Z
   gh pr create --title "Release vX.Y.Z" --body "..."
   ```

   Then walk away. The merge queue runs the full CI gate (`make lint`,
   `make test`, `make conformance`, `make lint-harn`, `make fmt-harn`,
   `make check-highlight`, `make check-language-spec`,
   `make check-trigger-quickref`, `make check-trigger-examples`,
   `make check-docs-snippets`, `verify_release_metadata.py`, portal
   lint+build, Windows smoke). Once it lands, Publish release fires.

## New-crate first-release pre-flight (harn#609)

**When this applies.** The pending release adds a new workspace crate
(e.g. `crates/harn-foo`) AND wires an already-published crate (most
commonly `harn-cli`) to depend on it via the standard
`harn-foo = { path = "../harn-foo", version = "0.7" }` pattern.

**Why it matters.** During `release_gate.sh audit` (run by `--prepare`),
the `package-audit` lane runs `scripts/verify_crate_packages.sh`, which
runs `cargo package -p harn-cli --no-verify`. Cargo strips the path dep,
replaces it with the version requirement, and queries crates.io to
validate it. If `harn-foo` has never been published, the lookup fails
with `no matching package named harn-foo found`. `--no-verify` only
skips the staged build, not dependency-resolution.

**Recommended pre-flight (do this BEFORE running `--prepare`):**

```bash
# From a clean worktree, seed the new crate at the current version.
cargo publish -p harn-foo --no-verify --allow-dirty
```

After this, every subsequent release flows through the consolidated PR
without intervention.

**Recovery path (if the prepare step fails in audit):**

Run `--prepare` with the bootstrap env var:

```bash
HARN_BOOTSTRAP_NEW_CRATES=1 ./scripts/release_ship.sh --prepare --bump patch
```

The flag tells `release_ship.sh` to skip the publish dry-run AND tells
`verify_crate_packages.sh` to skip the harn-cli package check. The bump
proceeds normally. After the consolidated PR lands, the Publish release
workflow's `cargo publish --workspace` orders intra-workspace deps
correctly and publishes `harn-foo` before `harn-cli`.

If finalize itself fails the same way, re-trigger it with the input set:

```bash
gh workflow run publish-release.yml -f bootstrap_new_crates=true
```

**For maintenance.** Add the new crate to:

- `scripts/publish.sh`'s `WORKSPACE_CRATES` array in dependency order
  (the per-crate fallback walks this list).
- Optionally, an explicit `cargo package -p harn-foo --allow-dirty
  --no-verify` step in `scripts/verify_crate_packages.sh` to catch
  packaging issues for the new crate as a separate audit signal.

## What happens automatically after the release PR lands

10. **Publish release** workflow (`.github/workflows/publish-release.yml`)
    detects tag drift (`Cargo.toml` ahead of latest `vX.Y.Z` tag) and runs
    `./scripts/release_ship.sh --finalize` under the App identity:
    portal-check + publish dry-run + push tag + `cargo publish` + render
    notes + create or update the GitHub release. **Audit is skipped** —
    the merge-queue CI just proved it.
11. The tag push triggers **Build release binaries** workflow
    (`.github/workflows/build-release-binaries.yml`), which builds darwin/linux × x86/arm
    binary tarballs, publishes a multi-arch GHCR container image, and
    attaches the binaries to the GitHub release.

## Recovery paths (don't reach for these unless something failed)

- **Finalize failed mid-run**: re-trigger from the Actions UI
  (workflow_dispatch). All scripts are idempotent — per-crate publish
  skips already-published, `ensure_tag_at_head` skips if the tag already
  points where it should, `gh release` is view-then-edit-or-create. Pass
  `reaudit: true` if you want it to re-run the full audit (slower; only
  needed if something on main has changed since the PR landed).
- **Build release binaries workflow needs to re-emit binaries** for an already-tagged
  version: `gh workflow run build-release-binaries.yml --ref main -f tag=vX.Y.Z`.
- **Accidentally landed a "Prepare vX.Y.Z release"-style commit on main
  without the consolidated bump**: the `Open version bump PR (recovery)`
  workflow exists for this. Trigger via `gh workflow run
  bump-release.yml` to open the historical bump PR pattern.
- **Truly stuck local recovery (rare)**: `./scripts/release_ship.sh
  --prepare --bump patch` from a fresh release branch, or
  `./scripts/release_ship.sh --finalize` from updated `main`.

## Source of truth

When in doubt, prefer the repo scripts over re-inventing the steps:

```bash
./scripts/release_ship.sh --prepare --bump patch     # consolidated prep
./scripts/release_ship.sh --finalize                  # only for local recovery
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
./scripts/release_gate.sh full --bump patch --dry-run   # all-in-one dry run
```

## Rules

- Stop on the first failed gate. Report the actual error.
- A real release has exactly one release commit on `main`, landed via
  PR/merge queue: `Release vX.Y.Z`. Author writes the changelog +
  code + docs + version bump in one shot via `--prepare`.
- Treat repo consistency as part of the release PR, not an optional
  cleanup pass. If behavior changes, update human-facing docs in the
  same release PR.
- If syntax / parser / lexer / tree-sitter changed, update
  `spec/HARN_SPEC.md` (the source of truth). The pre-commit hook
  regenerates `docs/src/language-spec.md` for you;
  `make check-language-spec` gates on the result in CI.
- The grammar/spec audit (run during `--prepare`) includes
  `scripts/verify_language_spec.py` (extracts ` ```harn ` fences and
  runs `harn check`) and `scripts/verify_tree_sitter_parse.py` (sweeps
  positive `.harn` programs through the executable tree-sitter
  grammar). Treat failures as spec drift, not just docs drift.
- `verify_release_metadata.py` (now wired into merge-queue CI) accepts
  either a matching state (`Cargo.toml == CHANGELOG top`, the new
  consolidated baseline) or one-bump-ahead (the legacy intermediate
  state).
- `release_ship.sh --finalize` pushes the tag **before** `cargo publish`
  so binary build / GHCR / downstream fetchers (e.g. `burin-code`'s
  `fetch-harn`) run in parallel with crates.io.
- The release-bot App needs `Contents: write`, `Pull requests: write`,
  `Actions: write`, `Metadata: read` on the repo. Repo secrets:
  `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`, `CARGO_REGISTRY_TOKEN`.
- `CHANGELOG.md` is the release-language source of truth. Notes are
  rendered from it via `scripts/render_release_notes.py`.

## Wall-clock expectations

`release_gate.sh audit` (run by `--prepare`) does a serial `cargo build
--workspace --all-targets` warm prebuild before spawning 5 parallel
lanes (`rust-audit`, `harn-audit`, `docs-audit`, `grammar-audit`,
`security-audit`). Typical wall-clock:

- Cold `target/`: ~6-10 min, dominated by prebuild.
- Warm `target/` after a recent build: ~10-30 s for the whole audit.
- A lane exceeding ~5 min after the prebuild is a regression worth
  investigating, not cold-cache cost.

In CI, the merge-queue CI of the Release PR pays cold-cache cost
(~10-15 min). The Publish release workflow no longer pays for an
audit (~7 min savings vs. the legacy two-PR flow).

## Useful shortcuts

```bash
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
