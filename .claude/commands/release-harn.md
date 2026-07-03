Run the merge-queue-safe Harn release workflow.

## TL;DR

The release is **one** human PR titled `Release vX.Y.Z`. It carries the
changelog, code, docs, AND the `Cargo.toml`/`Cargo.lock` version bump together.
After it lands through the merge queue, the **`vX.Y.Z` tag is pushed at the
release commit** — and *that tag push* (not the merge to `main`) is what triggers
the Publish release workflow to `cargo publish` to crates.io and kick off the
binary/container build.

> **`publish-release.yml` does NOT tag `main` HEAD for you.** This changed with
> the release-pipeline modernization (#2971–#2973). If the tag is missing when
> the release commit lands on `main`, the publish run fails with
> `Cargo.toml=X is ahead of latest tag vY, but vX does not exist. Push vX at the
> release commit`. The canonical orchestrator
> (`release_harn.harn --mode ship-pr`) pushes the tag as part of the ship; if you
> run the steps by hand you push the tag yourself (step 10). No second PR.

```text
human/agent: write & land "Release vX.Y.Z" PR
        │
        ▼  PR lands through merge queue (full audit ran in CI)
human/agent: push signed vX.Y.Z tag at the release commit
        │   (orchestrator ship-pr does this; or push it by hand — step 10)
        ▼  the TAG push (not the main push) triggers Publish release
bot:    Publish release runs cargo publish + creates the GH release
        │
        ▼  tag push cascades
bot:    Build release binaries builds binaries + multi-arch container
        │
        ▼
        v0.8.X is shipped (binaries, container, crates.io, release notes)
```

## What the human/agent owns

Steps 1-10 are the only steps that need judgment. After step 10 (the tag push)
you are done — do **not** run `release_ship.sh --finalize` locally as a default
step.

1. Branch off main: `git checkout -b release/vX.Y.Z`. The branch name is
   conventional; the publish keys on the `vX.Y.Z` tag, not the branch name.
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
   manifests, regenerates **version-embedded derived files** (highlight
   keywords, language-spec mirror, and the `spec/protocol-artifacts/*` — whose
   `manifest.json` `artifactVersion` and fixture runtime versions track the
   crate version), and `git add`s everything. Use `--skip-audit` to trust the
   merge-queue CI when iterating fast; use `--skip-dry-run` for the same
   reason on the publish dry-run.

   **Headless / hand-bumped fallback.** `release_ship.sh --prepare` refuses to
   run standalone (it requires `release_harn.harn`), and `release_harn.harn`'s
   live note generation needs `--agent` model creds — both absent in headless
   runs. If you bump the version *by hand* there (edit the `[workspace.package]`
   `version` in `Cargo.toml` + the workspace-crate `version =` lines in
   `Cargo.lock`), you MUST also regenerate the version-embedded derived files
   yourself, or the slow Release-PR merge-queue audit fails late on
   `check-protocol-artifacts` (`protocol artifacts are stale or missing`):

   ```bash
   # after bumping the version, from the release branch:
   cargo run --bin harn -- run scripts/sync_protocol_fixture_runtime_versions.harn -- --from <old> --to <new>
   CARGO_INCREMENTAL=0 make gen-protocol-artifacts   # rebuilds harn-cli at <new> so artifactVersion is right
   make check-protocol-artifacts                      # confirm before pushing
   ```

9. Commit, rebase onto latest `origin/main`, push, open the PR titled
   **`Release vX.Y.Z`**, and enable auto-merge:

   ```bash
   git commit -m "Release vX.Y.Z"
   git fetch origin main && git rebase origin/main
   # Resolve any CHANGELOG.md conflicts: bullets that landed on main while
   # --prepare was running may need to move from v0.7.52 → v0.7.53, or a
   # Merge Captain-style bullet may end up duplicated across both
   # sections. Verify v(X.Y.Z-1) section still matches the previous tag:
   #   diff <(git show v(X.Y.Z-1):CHANGELOG.md | awk '/^## v(X.Y.Z-1)/,/^## /') \
   #        <(awk '/^## v(X.Y.Z-1)/,/^## /' CHANGELOG.md)
   git push -u origin release/vX.Y.Z
   gh pr create --title "Release vX.Y.Z" --body "..."
   gh pr merge --auto         # merge-queue picks the strategy
   ```

   The merge queue runs the full CI gate (`make lint`,
   `make test`, `make conformance`, `make lint-harn`, `make fmt-harn`,
   `make check-highlight`, `make check-language-spec`,
   `make check-trigger-quickref`, `make check-trigger-examples`,
   `make check-docs-snippets`, `verify_release_metadata.py`, portal
   lint+build, Windows smoke). Auto-merge fires it as soon as CI is green.
   Then go to step 10 — the merge alone does **not** publish.

   **Why rebase before push:** `--prepare` takes ~1-15 min depending on
   cache state; main may have moved while it ran. Rebasing now (instead
   of waiting for the merge queue to push back) catches CHANGELOG drift
   while the context is fresh and avoids a stale PR sitting in queue.

10. **Push the `vX.Y.Z` tag at the release commit.** This is the step that
    actually ships — `publish-release.yml` will not tag `main` HEAD for you, so
    until the tag exists nothing is published (the post-merge `publish-release`
    run on `main` fails fast with `… but vX.Y.Z does not exist. Push vX.Y.Z at
    the release commit`). After the `Release vX.Y.Z` PR squash-merges, tag the
    resulting `main` commit:

    ```bash
    git fetch origin main --tags
    REL=$(git rev-parse origin/main)   # the squashed "Release vX.Y.Z (#N)" commit
    git tag -s vX.Y.Z "$REL" -m "Release vX.Y.Z"   # signed (org rulesets); -a also works
    git push origin vX.Y.Z
    ```

    The tag push (not the earlier main push) triggers `publish-release.yml` via
    its `tags: ['v*']` trigger, which skips drift detection and publishes from
    the tag. The canonical `release_harn.harn --mode ship-pr` orchestrator does
    this tag push for you; the manual sequence above is the fallback when you ran
    steps 1-9 by hand. **A transient red `publish-release` run on the `main` push
    is expected** — it's the "tag missing" guard firing before you push the tag;
    pushing the tag is what ships, and that run is the real signal.

   **Why `--auto`:** the merge queue gates a substantial CI suite, so
   the release sits in queue for ~10-15 min cold-cache. Auto-merge means
   you don't have to babysit it; it lands when CI goes green. Don't pass
   `--squash`/`--merge`/`--rebase` — the merge queue's branch protection
   rules pick the strategy and `gh` will reject explicit overrides.

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

## What happens automatically after you push the tag (step 10)

11. **Publish release** workflow (`.github/workflows/publish-release.yml`) fires
    on the `vX.Y.Z` **tag push** (its `tags: ['v*']` trigger), skips drift
    detection, and runs `./scripts/release_ship.sh --finalize` under the App
    identity: portal-check + publish dry-run + `cargo publish` + render notes +
    create or update the GitHub release. **Audit is skipped** — the merge-queue
    CI just proved it. Note: `--finalize` no longer *creates* the tag — the tag
    you pushed in step 10 is the trigger and the source of truth for what
    publishes (`ensure_tag_at_head` is a no-op when the tag already points at
    HEAD). The separate `main`-push run of `publish-release.yml` is only a guard:
    it errors if the tag is missing or points elsewhere and never tags `main`
    HEAD itself.
12. The tag push also triggers **Build release binaries** workflow
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
- The `vX.Y.Z` tag is pushed in **step 10** (by `release_harn.harn --mode
  ship-pr`, or by hand), *before* `publish-release.yml` runs `cargo publish`, so
  binary build / GHCR / downstream fetchers (e.g. `burin-code`'s `fetch-harn`)
  run in parallel with crates.io. `release_ship.sh --finalize` no longer pushes
  the tag — it publishes from the existing tag (`ensure_tag_at_head` no-ops when
  the tag already points at HEAD).
- The release-bot App needs `Contents: write`, `Pull requests: write`,
  `Actions: write`, `Metadata: read` on the repo. Repo secrets:
  `RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`, `CARGO_REGISTRY_TOKEN`.
- `CHANGELOG.md` is the release-language source of truth. Notes are
  rendered from it via `scripts/render_release_notes.py`.

## Wall-clock expectations

`release_gate.sh audit` (run by `--prepare`) does a serial `cargo build
-p harn-cli --bin harn` warm prebuild before spawning the parallel lanes
(`rust-audit`, `harn-audit`, `docs-audit`, `grammar-audit`, `security-audit`,
`package-audit`, `smoke-audit`). Typical wall-clock:

- Cold `target/`: ~10-15 min, dominated by `rust-audit` clippy/nextest and
  package verification rather than the CLI prebuild.
- Warm `target/` after a recent build: ~2-5 min for the whole audit.
- A lane exceeding ~5 min after the CLI prebuild is a regression worth
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
