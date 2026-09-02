# AGENTS.md

Harn is a programming language and runtime for orchestrating AI agents.

This file is the repository contract for Codex, Claude, and other coding
agents. `CLAUDE.md` must remain a symlink to it. The durable design principles
live in [Engineering principles](docs/src/dev/engineering-principles.md):

- ambitious outcomes behind boring seams;
- one owner with many projections;
- structure over prose;
- evidence matched to claims;
- canonical paths;
- controllable autonomy;
- cross-surface convergence;
- operationally complete launches.

## Ownership

- Harn owns orchestration, transcript lifecycle, replay/eval, delegated-worker
  lineage, capability policy, and mutation-session audit metadata.
- Hosts own native presentation, approval UX, concrete file mutations, and
  undo/redo semantics.
- Prefer the Harn runtime and stdlib over new host glue.
- A semantic decision has one owner. CLI, TUI, IDE, headless, and cloud
  behavior should be projections or adapters, not parallel implementations.
- Prefer deep modules: small interfaces that hide substantial behavior.
- Make contracts structural with types, registries, events, generated
  projections, and drift checks. Prose should explain the contract, not enforce
  it.

## Working agreement

- Research, implement, verify, recover, and ship autonomously inside the
  approved scope.
- Pause for genuine ambiguity, destructive or out-of-scope action, production
  impact, exceptional spend, or new authority—not routine reversible work.
- Treat stop, wait, stand down, and pivot as control events. Do not continue
  stale work after one arrives.
- Start from the claim and name a plausible falsifier. Match evidence to the
  claim; test counts alone are not proof.
- Exercise the canonical user path for product claims and progress,
  interruption, recovery, and terminal state for liveness claims.
- Use multiple trials and calibrated graders for stochastic quality claims.
- "Ship" means landed on the intended main branch with required post-merge or
  release checks complete.

## Harn scripts

- Before writing or editing `.harn`, run `harn skill list --json`.
- Fetch the narrowest guide with `harn skill get <name> --full`.
- Use `harn-language` for syntax, modules, types, and `llm_call`;
  `harn-orchestration` for triggers, workers, personas, and agent loops;
  `harn-testing` for fixtures; and `harn-product-quality` for user-facing flows.
- Fallback references are `docs/llm/harn-quickref.md` and
  `docs/llm/harn-triggers-quickref.md`.
- Default new scripts to `fn main(harness: Harness) { ... }`.
- Route capabilities through `harness.*`.
- Claude Code gets the same version-matched reminder through
  `.claude/skills/harn-scripting/SKILL.md`.

## Setup and resource isolation

- Run `make setup` on a fresh clone and in every new worktree. It installs
  hooks and tools, configures sccache and a private target directory, then runs
  a locked build of the canonical Harn CLI so later product-path commands and
  focused tests reuse the same linked dependency graph.
- A raw `git worktree add` has no `.cargo/config.toml`; without setup, cold
  Cargo probes can time out. Reuse an existing binary with
  `HARN_BIN=<path> HARN_BIN_NO_BUILD=1` or explicitly allow a cold probe with
  `HARN_BIN_CARGO_TIMEOUT_SECONDS=3600`. Only when the compiler wrapper is
  genuinely wedged, opt into one wrapper-disabled retry with
  `HARN_BIN_RETRY_WITHOUT_WRAPPER=1`.
- Never share a mutable Cargo `target-dir` or `build-dir` across concurrent
  worktrees. Every setup profile derives one stable per-worktree target under
  `${XDG_CACHE_HOME:-$HOME/.cache}/harn/dev-setup/harn-target/` and configures
  sccache for compiler-object reuse. Because released sccache versions include
  Rust's absolute compilation directory in the cache key, setup also restores
  an immutable toolchain-keyed Cargo target seed with filesystem copy-on-write.
  The seed keeps third-party dependency artifacts but removes workspace
  binaries, fingerprints, dep-info, and incremental state before publication
  and after restore, so every lane rebuilds Harn against its own source tree.
  Each lane gets private files; unsupported filesystems simply take the cold
  build path. A successful canonical CLI build publishes the seed once; an 8
  GiB ceiling prevents an accumulated multi-profile target from becoming the
  permanent seed.
- `HARN_DEV_TARGET_WORKTREE_PATH` and `CODEX_WORKTREE_PATH` must name the
  checkout being configured. Setup ignores stale sibling-worktree values.
- Setup phases are fingerprinted under `.codex/dev-setup/`; use
  `HARN_DEV_SETUP_FORCE=1 make setup` only to refresh them.
- Codex staging worktrees use `make setup-bootstrap`; run `make setup` or
  `make setup-rust` in the final task worktree.
- Claude session startup delegates to the same setup path through
  `scripts/claude-dev-setup-once.sh`. It blocks only on the `bootstrap` profile,
  and warms the compiling phases in the background. An early build can therefore
  wait on that warm. `.claude/dev-setup/latest.log` records it.
- Keep installed hooks on. Use `HARN_HOOKS_FULL_LOCAL=1` for build-backed local
  gates and `HARN_PREPUSH_FULL_TESTS=1` for the broader pre-push suite.
- The shared Codex and Claude shell guard rejects raw Cargo build/test commands
  and build output piped directly into filters. Use Make targets; redirect
  output to a file before filtering. `HARN_ALLOW_RAW_CARGO=1` is the explicit
  one-off escape. See [Agent shell guard](docs/src/dev/agent-shell-guard.md).

## Repository map

- `crates/harn-lexer`, `crates/harn-parser`: tokens, AST, parser, type checker.
- `crates/harn-stdlib`, `crates/harn-vm`: embedded stdlib, compiler, VM,
  providers, orchestration, transcripts, bridge, and ACP.
- `crates/harn-cli`: CLI, conformance, portal server, MCP/OAuth, A2A/ACP,
  replay, and eval tooling.
- `crates/harn-lint`, `crates/harn-fmt`, `crates/harn-lsp`,
  `crates/harn-dap`: language tooling and editor/debugger integration.
- `crates/harn-cli/portal/`: React/Vite persisted-run UI.
- `conformance/tests/`: executable language/runtime specification.
- `spec/chapters/*.md`: canonical spec sources.
- `docs/src/`, `website/`: documentation sources and harnlang.com.
- `tree-sitter-harn/`, `editors/vscode/`: grammar and VS Code extension.
- `crates/harn-kernel`: canonical compiler, versioned program artifact,
  deterministic portable runtime, and runtime type contract.
- `crates/harn-wasm`: workspace-owned browser adapter; verify its generated
  bindings, authority imports, and real worker path with `make wasm-check`.

## Source ownership and generated files

- Edit `spec/chapters/*.md`, then run `make sync-language-spec`; do not edit
  generated `spec/HARN_SPEC.md` or `docs/src/language-spec.md`.
- Generate `docs/theme/harn-keywords.js` with `make gen-highlight`.
- Generate protocol artifacts with `make gen-protocol-artifacts`; only
  `spec/protocol-artifacts/*_test.go` is hand-edited.
- `scripts/generated_artifacts.toml` is the source of truth for every gen/check
  pair and every `check-*` preflight classification. A new artifact needs
  Makefile gen/check targets, a registry entry, `make all`, and CI wiring.
- Classify checks as `source`, `binary`, or `excluded`. Source checks read
  committed files; binary checks require a fresh Harn executable.
- Generated/local paths include `docs/dist/`, `.harn-runs/`, `.harn/`,
  `.harn/receipts/`, `.claude/`, `.burin/`, `target/`, and `node_modules/`.
- The prompt-template engine is
  `crates/harn-vm/src/stdlib/template.rs`. Host and script rendering both use
  `render_template_result`; do not add another parser or evaluator.
- Preserve pre-v2 `{{name}}` missing-identifier passthrough. New constructs
  fail with parse errors. Vocabulary lives in
  `crates/harn-vm/src/stdlib/template/vocabulary.rs`; regenerate with
  `make gen-prompt-grammar`.
- Keep stdlib registration authoritative. Register builtins with
  `#[harn_builtin]`; linter and editor awareness derive from the live stdlib.
- Public stdlib functions need explicit return types: named closed records,
  `Result<T, E>`, or typed maps rather than `any` or open `dict`.

## Verification

- Start with the narrowest check through the owning interface.
- Run one exact Rust test without unrelated nextest discovery with
  `HARN_TEST_ONE_NAME='module::tests::case' make test-one`. Set
  `HARN_TEST_ONE_PACKAGE` only when the test is outside `harn-cli`, and
  `HARN_TEST_ONE_BINARY` when the test lives in the package's `tests/`
  directory rather than its `src/`. A name the requested target does not define
  is refused before the run; zero matches from one that does fail loudly.
- Workspace tests: `make test` (requires cargo-nextest; `make setup` installs it).
- Full gate: `make all`.
- Before declaring a change clean, run `make check-drift` and inspect
  `git status`. After Rust registry or executable-semantics changes, rebuild
  and run `make check-drift-binary`.
- Syntax/parser/keyword changes need conformance coverage, `make conformance`,
  `make lint-harn`, `make fmt-harn`, and tree-sitter tests.
- Docs code blocks need `make check-docs-snippets`.
- Portal changes need `npm run portal:lint`, `npm run portal:test`, and
  `npm run portal:build`.
- VS Code changes need `(cd editors/vscode && npm run compile)`.
- Tree-sitter changes need `(cd tree-sitter-harn && npm test)`.
- Do not add real-time sleeps, wall-clock polling, `SystemTime::now()`, or short
  `recv_timeout` calls to tests. Use paused Tokio time, `EventLog::subscribe()`,
  or `OrchestratorHarness`; see `docs/src/dev/testing.md`.

## Cross-surface changes

- Syntax changes usually touch lexer, parser, spec, tree-sitter, and
  conformance.
- Runtime/builtin changes usually touch VM, CLI, docs, README, changelog, and
  conformance.
- Public CLI, builtin, or host-capability changes require user-facing docs and
  help.
- Prompt syntax changes require template conformance fixtures, changelog,
  vocabulary regeneration, and VS Code grammar verification.
- For autonomous/background edits, prefer worktree-backed execution over
  ambient working-directory state.

## Editing and changelog

- Use the simplest safe edit. Prefer `std/edit` when structural addressing,
  cross-file rename semantics, or staged hash-guarded preview reduces risk;
  normal patch tools are appropriate for ordinary maintenance.
- Non-trivial PRs add one `changelog.d/<id>.<category>.md` fragment. Categories
  are `breaking`, `added`, `changed`, `deprecated`, `removed`, `fixed`, and
  `security`. See `changelog.d/README.md`.
- Use `no-changelog-needed` only after the soft gate fires on a change with no
  user-visible impact.

## Pull requests

- Title pull requests `[Area] Sentence case description`, using one tag from
  the table in [CONTRIBUTING.md](CONTRIBUTING.md#title-format). Release pull
  requests stay exactly `Release vX.Y.Z`; `publish-release.yml` matches that
  subject. Bot titles are left alone.
- Keep the description to roughly five sentences: what changed in behavior
  terms, why, the one risk or blind spot, and how you verified the claim at the
  level of the claim. Do not restate the Files or Checks tabs.
- Name the sub-asks a pull request closes with `Closes #N items: 1, 3`, or
  `Single-ask: #N` when the issue is not enumerated.

## Release

- Run live releases only through the `hosted-release.yml` workflow on
  `burin-labs/harn-bump-fleet`, pinned to an exact current `origin/main` SHA,
  and approve its protected `release` environment. Do not run the local
  harness or `scripts/release_ship.sh` for a normal live release.
- After the tag exists, resume durable post-tag proof from `harn-bump-fleet`
  with
  `scripts/watch_harn_release.sh --tag vX.Y.Z --repo <harn-checkout> --yes-live-release`.
- Run the watcher from the `harn-bump-fleet` checkout so its pinned runtime,
  environment loader, release lease, and cleanup authority stay canonical.
  Completion requires the release PR, complete asset manifest, main-cache
  warm, and transient-ref cleanup—not just a visible tag or GitHub release.
- Dry-run the full release gate with
  `./scripts/release_gate.sh full --bump patch --dry-run`.
- Dry-run crate publishing with `./scripts/publish.sh --dry-run`.

## Merge overrides

- Rare founder overrides for CI incidents, merge-queue cost spikes, or
  fix-forward lands use the org-admin labels `bypass-ci`,
  `bypass-merge-queue`, and `force-merge`.
- Labels are a trigger only. The workflow re-checks organization or repository
  admin permission and refuses fork PRs. See
  [Merge overrides](docs/src/dev/merge-overrides.md) and the
  [`burin-labs/.github` README](https://github.com/burin-labs/.github#merge-overrides).
- Prefer the normal merge queue whenever it is cheap enough.

<!-- BEGIN HARN SHARED AGENT CONTRACT: managed by harn-bump-fleet -->

## Ecosystem working agreement

- Pursue the ambitious product outcome; make the seams boring with small typed
  interfaces, explicit invariants, and deterministic projections.
- Give each behavior one semantic owner. Generate or parity-test other surfaces
  instead of maintaining competing implementations.
- Work autonomously inside approved scope. Pause for destructive, production,
  high-spend, ambiguous, or authority-expanding actions—not routine reversible work.
- Treat stop, wait, stand down, and pivot as control events for long-lived work.
- Match evidence to the claim: exercise the canonical user path, state the
  falsifier, verify liveness and recovery, and record residual blind spots.
- "Ship" means landed on main with required deploy and post-merge checks complete.

<!-- END HARN SHARED AGENT CONTRACT -->
