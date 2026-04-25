Run the merge-queue-safe Harn release workflow.

## TL;DR

The **only** part of a release that requires human/agent intuition is writing
and landing the **"Prepare vX.Y.Z release"** PR. Everything from there is
automated:

```text
land "Prepare vX.Y.Z release" PR
        │
        ▼  Bump Release workflow auto-fires
   opens "Bump version to X.Y.Z" PR
        │
        ▼  PR lands through merge queue
   Finalize Release workflow auto-fires
        │
        ▼  pushes vX.Y.Z tag (under release-bot App identity)
   Release workflow auto-fires
        │
        ▼  builds binaries + multi-arch container, populates GH release
   v0.7.X is shipped
```

You write the prepare PR. Walk away. The bot opens the bump PR. The merge
queue lands it. The bot tags, publishes to crates.io, builds binaries,
publishes the container, and creates the GitHub release with rendered notes.

## What the human/agent owns

Step 1-8 below are the only steps that need judgment. After step 8 you are
done — do **not** run `release_ship.sh` locally as a default step.

1. Inspect the worktree with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless the
   user scopes the release more narrowly.
2. Read enough diff context to summarize the pending work accurately.
3. Audit all pending changes for code quality, correctness, and test
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
     the final gate before committing.
   Do not skip this step — shipping untested or buggy code is worse than
   delaying a release.
4. Repo-consistency sweep. Update release-facing docs and operator guidance
   as needed: `README.md`, `CLAUDE.md`, `docs/src/`, `spec/HARN_SPEC.md`,
   `CHANGELOG.md`, and developer-setup surfaces (`scripts/dev_setup.sh`,
   `Makefile`, `.githooks/`, `CONTRIBUTING.md`, `docs/src/portal.md`).
5. If syntax, parser, lexer, or tree-sitter changed, update
   `spec/HARN_SPEC.md` first — it is the formal language-spec source of
   truth.
6. Update `CHANGELOG.md` with a new top entry `## vX.Y.Z` describing the
   actual pending code changes that will ship. The version chosen here
   (patch / minor / major bump from the current Cargo.toml version) drives
   what the bot opens later — pick deliberately.
7. Run `cargo fmt --all` once so the prep PR is formatting-clean.
   `release_gate.sh audit` runs `cargo fmt -- --check` and will reject drift
   later.
8. Stage, commit, push, and open the **"Prepare vX.Y.Z release"** PR.
   Include every file that ships in this version: code, docs,
   `CHANGELOG.md`. Do **not** include `Cargo.toml` / `Cargo.lock` version
   bumps in this PR — the bot's bump PR carries those.

## New-crate first-release pre-flight (harn#609)

**When this applies.** The pending release adds a new workspace crate
(e.g. `crates/harn-foo`) AND wires an already-published crate (most
commonly `harn-cli`, but any of the published members) to depend on it
via the standard `harn-foo = { path = "../harn-foo", version = "0.7" }`
pattern.

**Why it matters.** During the audit's `package-audit` lane,
`scripts/verify_crate_packages.sh` runs `cargo package -p harn-cli
--no-verify`. Cargo strips the path dep, replaces it with the version
requirement, and queries crates.io to validate it. If `harn-foo` has
never been published, the lookup fails with `no matching package named
harn-foo found`. `--no-verify` only skips the staged build, not the
dependency-resolution step that fails here. The Bump Release workflow
audit therefore aborts before the publish dry-run ever runs.

**Recommended pre-flight (do this BEFORE landing the prepare PR):**

```bash
# From a clean worktree at main HEAD (or the prepare branch), seed
# the new crate at the current workspace version.
cargo publish -p harn-foo --no-verify --allow-dirty
```

After this, every subsequent release goes through the normal automated
flow without intervention. Confirm crates.io picked it up
(`https://crates.io/crates/harn-foo`) before merging the prepare PR.

**Recovery path (use when the prepare PR already landed and the bump
workflow is failing in audit):**

1. Manually re-trigger Bump Release with the bootstrap input:

   ```bash
   gh workflow run bump-release.yml \
     -f bootstrap_new_crates=true
   ```

2. The flag sets `HARN_BOOTSTRAP_NEW_CRATES=1`, which tells
   `release_ship.sh` to skip the publish dry-run AND tells
   `verify_crate_packages.sh` to skip the harn-cli package check. The
   bump PR opens normally.
3. Land the bump PR through the merge queue. If Finalize Release also
   fails the same way, re-trigger it the same way:

   ```bash
   gh workflow run finalize-release.yml \
     -f bootstrap_new_crates=true
   ```

   The real `cargo publish --workspace` inside finalize orders
   intra-workspace deps correctly and will publish `harn-foo` before
   `harn-cli`.

**For maintenance.** Add the new crate to:

- `scripts/publish.sh`'s `WORKSPACE_CRATES` array in dependency order
  (the per-crate fallback walks this list when the workspace publish
  bails mid-stream).
- Optionally, an explicit `cargo package -p harn-foo --allow-dirty
  --no-verify` step in `scripts/verify_crate_packages.sh` to catch
  packaging issues for the new crate as a separate audit signal
  (see the existing `harn-hostlib` step for the pattern).

## What happens automatically after the prepare PR lands

9. **Bump Release** workflow (`.github/workflows/bump-release.yml`) fires on
   push to `main` when the head commit starts with `Prepare v`. It:
   - Reads `CHANGELOG.md`'s top `## vX.Y.Z` entry, compares to
     `Cargo.toml`'s current version, derives the bump type (patch / minor /
     major) via `scripts/detect_bump_type.py`.
   - Runs `./scripts/release_ship.sh --bump <type>` under the
     `harn-release-bot` App identity: audit, dry-run publish, bump
     `Cargo.toml` + `Cargo.lock`, commit `Bump version to X.Y.Z`, push
     `release/vX.Y.Z`, open the bump PR.
10. The bump PR's CI runs (because the App-pushed branch isn't suppressed
    the way `GITHUB_TOKEN`-pushed branches are). Once green, the merge
    queue lands it.
11. **Finalize Release** workflow (`.github/workflows/finalize-release.yml`)
    fires on push to `main` when the head commit starts with `Bump version
    to`. It runs `./scripts/release_ship.sh --finalize` under the App
    identity: audit, dry-run publish, push the tag, `cargo publish`, render
    notes from `CHANGELOG.md`, create/update the GitHub release.
12. The tag push triggers **Release** workflow
    (`.github/workflows/release.yml`), which builds darwin/linux × x86/arm
    binary tarballs, publishes a multi-arch GHCR container image, and
    attaches the binaries to the GitHub release.

## Recovery paths (don't reach for these unless something failed)

- A workflow failed mid-run: **re-trigger from the Actions UI** (each
  workflow exposes `workflow_dispatch`). All scripts are idempotent —
  per-crate publish skips already-published, `ensure_tag_at_head` skips
  if the tag already points where it should, `gh release` is
  view-then-edit-or-create.
- The Release workflow needs to re-emit binaries for an already-tagged
  version: `gh workflow run release.yml --ref main -f tag=vX.Y.Z`. The
  workflow accepts the tag input and runs at that tagged code.
- A truly stuck local recovery (rare): `./scripts/release_ship.sh
  --bump patch` or `./scripts/release_ship.sh --finalize` from updated
  `main`.

## Source of truth

When in doubt, prefer the repo scripts over re-inventing the steps:

```bash
./scripts/release_ship.sh --bump patch         # only for local recovery
./scripts/release_ship.sh --finalize           # only for local recovery
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
./scripts/release_gate.sh full --bump patch --dry-run   # all-in-one dry run
```

## Rules

- Stop on the first failed gate. Report the actual error.
- A real release has exactly two release commits on `main`, both landed via
  PR/merge queue: `Prepare vX.Y.Z release` (code + docs + `CHANGELOG.md`)
  followed by `Bump version to X.Y.Z` (Cargo.toml + Cargo.lock only). The
  human/agent creates the first; the bot creates the second.
- Treat repo consistency as part of the prepare PR, not an optional
  cleanup pass. If behavior changes, update human-facing docs in the same
  prep PR.
- If syntax / parser / lexer / tree-sitter changed, update
  `spec/HARN_SPEC.md` (the source of truth) and run
  `scripts/sync_language_spec.sh` to regenerate the docs mirror.
- The grammar/spec audit includes `scripts/verify_language_spec.py`
  (extracts ` ```harn ` fences and runs `harn check`) and
  `scripts/verify_tree_sitter_parse.py` (sweeps positive `.harn`
  programs through the executable tree-sitter grammar). Treat failures
  as spec drift, not just docs drift.
- `verify_release_metadata.py` accepts the pre-bump state — it passes
  when the top `CHANGELOG.md` entry is exactly one patch / minor / major
  step ahead of `Cargo.toml`. The Bump Release workflow relies on this.
- `release_ship.sh --finalize` pushes the tag **before** `cargo publish`
  so binary build / GHCR / downstream fetchers (e.g. `burin-code`'s
  `fetch-harn`) run in parallel with crates.io. Both the local script
  and the Finalize Release workflow honor this ordering.
- The release-bot App needs `Contents: write`, `Pull requests: write`,
  `Actions: write`, `Metadata: read` on the repo. Repo secrets:
  `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`, `CARGO_REGISTRY_TOKEN`.
- `CHANGELOG.md` is the release-language source of truth. Notes are
  rendered from it via `scripts/render_release_notes.py`.

## Audit wall-clock expectations

`release_gate.sh audit` runs a serial `cargo build --workspace
--all-targets` warm prebuild before spawning 5 parallel lanes
(`rust-audit`, `harn-audit`, `docs-audit`, `grammar-audit`,
`security-audit`). Typical wall-clock:

- Cold `target/`: ~6-10 min, dominated by prebuild.
- Warm `target/` after a recent build: ~10-30 s for the whole audit.
- A lane exceeding ~5 min after the prebuild is a regression worth
  investigating, not cold-cache cost.

In CI, both Bump Release and Finalize Release pay cold-cache audit cost
(~8-12 min total). The audit + publish wall-clock is typical for a
full release run.

## Useful shortcuts

```bash
# All-in-one dry run, stops before any destructive action:
./scripts/release_gate.sh full --bump patch --dry-run

# Render the GitHub release body locally from CHANGELOG.md:
./scripts/release_gate.sh notes

# Manually re-trigger any of the workflows:
gh workflow run bump-release.yml --ref main
gh workflow run finalize-release.yml --ref main
gh workflow run release.yml --ref main -f tag=vX.Y.Z
```
