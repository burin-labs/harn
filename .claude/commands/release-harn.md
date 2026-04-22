Run the merge-queue-safe Harn release workflow from the repo source of truth.

## TL;DR

```bash
# From updated main, after the release-content PR has landed:
./scripts/release_ship.sh --bump patch
```

`release_ship.sh --bump patch` handles audit, dry-run publish, version bump,
branch push, and automated bump PR creation. After that PR lands through the
merge queue, **the `Finalize Release` GitHub Action (`.github/workflows/finalize-release.yml`) runs `release_ship.sh --finalize` automatically** — it
audits, tags, publishes to crates.io, and creates/updates the GitHub release.
The human release path ends at opening the bump PR; the tag + publish are
automated. Only run `./scripts/release_ship.sh --finalize` locally to recover
from a failed workflow run, or manually re-trigger the workflow via "Run
workflow" on the `Finalize Release` GitHub Actions page.

## Default workflow

1. Inspect the worktree with `git status --short` and `git diff --stat`.
   Treat tracked and untracked changes as candidate release content unless the
   user scopes the release more narrowly.
2. Read enough diff context to summarize the pending work accurately.
3. Audit all pending changes for code quality, correctness, and test coverage.
   For each changed module or feature, check whether existing Rust tests
   and conformance tests (`conformance/tests/`) adequately cover the new or
   changed behavior. Fill gaps:
   - Add or update `#[test]` functions for new/changed Rust logic.
   - Add or update `.harn` + `.expected` conformance test pairs for any
     user-visible behavior changes or new builtins/features.
   - Fix implementation bugs, edge cases, or incomplete code paths discovered
     during the audit.
   - **Targeted tests first**: run tests only for changed crates during the
     audit loop (e.g. `cargo nextest run -p harn-vm` or
     `cargo test -p harn-vm`). This keeps the edit-test cycle fast.
   - **Full gate last**: once the audit is complete and all targeted tests
     pass, run `make test` (nextest when available, `cargo test --workspace`
     as fallback) and `cargo run --bin harn -- test conformance` as the final
     gate before committing.
   Do not skip this step — shipping untested or buggy code is worse than
   delaying a release.
4. Do a repo-consistency sweep before shipping. Update release-facing docs and
   operator guidance as needed, especially `README.md`, `CLAUDE.md`,
   `docs/src/`, `spec/HARN_SPEC.md`, `CHANGELOG.md`, and any developer setup
   surfaces such as `scripts/dev_setup.sh`, `Makefile`, `.githooks/`,
   `CONTRIBUTING.md`, and `docs/src/portal.md`.
5. If syntax, parser, lexer, or tree-sitter changed, update
   `spec/HARN_SPEC.md` first. Treat it as the formal language-spec source of
   truth.
6. Update `CHANGELOG.md` before bumping the version. The new top entry must
   describe the actual pending code changes that will ship.
7. Run `cargo fmt --all` once so the upcoming release content PR is
   formatting-clean. `release_gate.sh audit` runs `cargo fmt -- --check` and
   will reject drift later; catching it here avoids re-doing commits.
8. Stage, commit, push, and open a "Prepare vX.Y.Z release" PR. Include every
   file that ships in this version, including `CHANGELOG.md` and doc updates.
   Do **not** include `Cargo.toml` / `Cargo.lock` version bumps in this PR —
   the ship script produces those in a separate "Bump version to X.Y.Z" PR.
9. After the release-content PR lands through the merge queue, sync `main` and
   run `./scripts/release_ship.sh --bump patch` (or `minor`/`major`). The
   script audits, dry-run publishes, bumps `Cargo.toml`, commits the bump,
   pushes `release/vX.Y.Z`, and opens the bump PR.
10. After the bump PR lands through the merge queue, the `Finalize Release`
    GitHub Action runs `./scripts/release_ship.sh --finalize` automatically
    against updated `main`. It audits, dry-run publishes, creates/pushes the
    tag, publishes to crates.io, and creates/updates the GitHub release. Do
    not run `--finalize` locally as a default step; only do so if the
    workflow fails and the tree needs to be recovered manually. The
    workflow also exposes a `workflow_dispatch` trigger for manual
    re-runs from the GitHub Actions UI.
11. For an all-in-one dry run that stops before any destructive action,
    use `./scripts/release_gate.sh full --bump patch --dry-run`.
12. Only drop down to the piecewise `release_gate.sh prepare` / `publish` /
    `notes` commands when `release_ship.sh` cannot do what you need
    (e.g. recovering a partial release).

## Source of truth

Always prefer the repo scripts:

```bash
./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...
./scripts/release_ship.sh --bump patch
./scripts/release_ship.sh --finalize
```

Do not re-invent the release ritual from memory if the script can do it.
Use normal git commands for the parts that the release gate intentionally does
not automate, such as analyzing pending local work and opening the
"Prepare vX.Y.Z release" PR. Once that PR lands and the tree is clean, prefer
`./scripts/release_ship.sh --bump patch` for the bump PR and
`./scripts/release_ship.sh --finalize` for tag/publish mechanics.

## Rules

- Report failures clearly and stop on the first failed gate.
- Summarize the resulting version, bump PR or publish status, release notes,
  and required tag/release follow-up.
- If `mdbook` is not installed, mention that the docs audit skipped mdBook build.
- If the tree is dirty, do not work around it silently for `prepare`; either
  stop or commit the intended release content first.
- If untracked files exist, call them out explicitly and decide whether they
  belong in the release before staging them.
- Treat repo consistency as part of the release gate, not an optional cleanup
  pass. If behavior changes, update human-facing docs in the same release when
  they describe that behavior.
- If local development setup changed, keep `README.md`, `CONTRIBUTING.md`,
  `scripts/dev_setup.sh`, and `Makefile` aligned so the bootstrap path stays
  obvious and low-friction.
- If observability surfaces changed, update `docs/src/portal.md` and any CLI
  references that describe `harn portal`.
- If grammar-related files changed, mention whether `spec/HARN_SPEC.md` was
  updated in the same batch.
- The grammar/spec audit includes `scripts/verify_language_spec.py`, which
  extracts `harn` fences from `spec/HARN_SPEC.md` and runs `harn check` on
  them. Treat failures there as spec drift, not just docs drift.
- The grammar/spec audit also includes `scripts/verify_tree_sitter_parse.py`,
  which sweeps positive `.harn` programs through the executable tree-sitter
  grammar. Treat failures there as parser/grammar divergence.
- A real release has exactly two release commits on `main`, both landed via
  PR/merge queue: `Prepare vX.Y.Z release` (code + docs + `CHANGELOG.md`)
  followed by `Bump version to X.Y.Z` (Cargo.toml + Cargo.lock only).
  `release_ship.sh --bump patch` creates and opens the second PR
  automatically; the human/agent creates the first.
- `scripts/release_ship.sh --bump patch` assumes the real release content,
  including docs consistency updates, has already landed through the merge
  queue and the local `main` tree is clean before it starts.
- `verify_release_metadata.py` accepts the pre-bump state — it passes when
  the top `CHANGELOG.md` entry is exactly one patch/minor/major step ahead
  of `Cargo.toml`. That means running `release_ship.sh --bump patch` on a
  "Prepare vX.Y.Z release" commit is fine even though Cargo.toml still points
  at the previous version.
- `release_ship.sh --finalize` pushes the tag **before** calling
  `cargo publish`, so GitHub release binary workflows and downstream fetchers
  (e.g. `burin-code`'s `fetch-harn`) start working in parallel with crates.io
  publication. The GitHub release body is created last so it can reference the
  final crates.io state.
- Finalize runs inside GitHub Actions via
  `.github/workflows/finalize-release.yml`. The workflow fires on push to
  `main` when the head commit starts with the prefix `Bump version to` (the
  convention `release_ship.sh --bump` writes), and also exposes
  `workflow_dispatch` for manual re-runs. It uses the repo secret
  `CARGO_REGISTRY_TOKEN` for crates.io and the default `GITHUB_TOKEN` for
  tag push + release creation. Running `release_ship.sh --finalize`
  locally is only needed when the workflow fails and a human has to
  recover — it is idempotent once the tag exists at HEAD.
- `CHANGELOG.md` is the release-language source of truth. Use
  `scripts/render_release_notes.py` or `./scripts/release_gate.sh notes` to
  produce the exact GitHub release body from it.

## Audit wall-clock expectations

`release_gate.sh audit` runs a serial `cargo build --workspace --all-targets`
warm prebuild before spawning the 5 parallel lanes (`rust-audit`,
`harn-audit`, `docs-audit`, `grammar-audit`, `security-audit`). The prebuild
removes cargo-lock contention that historically made `harn-audit`'s lint
phase stretch to ~12 min while fighting `rust-audit`'s clippy+nextest for
`target/`. Typical wall-clock:

- Cold `target/`: ~6-10 min, dominated by prebuild.
- Warm `target/` after a recent build: ~10-30 s for the whole audit.
- If any lane exceeds ~5 min after the prebuild, that is a regression —
  check for a new `cargo`-shelling hotspot inside that lane rather than
  assuming cold-cache cost.

## Useful shortcuts

```bash
./scripts/release_ship.sh --bump patch               # open the bump PR
./scripts/release_ship.sh --finalize                 # recovery only
./scripts/release_gate.sh full --bump patch --dry-run
./scripts/release_gate.sh notes

# Manual re-trigger of the finalize workflow:
gh workflow run finalize-release.yml --ref main
```
