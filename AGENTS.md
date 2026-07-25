# AGENTS.md

Harn is a programming language and runtime for orchestrating AI agents.

Use this file for repo-local rules. Keep generated files generated. Prefer the
existing Harn runtime and stdlib over new host glue.

## Harn scripts

- Before writing or editing `.harn`, run `harn skill list --json`.
- Fetch the narrowest guide with `harn skill get <name> --full`.
- Use `harn-language` for syntax, modules, types, and `llm_call`.
- Use `harn-orchestration` for triggers, workers, personas, and agent loops.
- Fallback docs: `docs/llm/harn-quickref.md` and
  `docs/llm/harn-triggers-quickref.md`.
- Default new scripts to `fn main(harness: Harness) { ... }`.
- Route capabilities through `harness.*`.

Claude Code users get the same reminder through
`.claude/skills/harn-scripting/SKILL.md`; the full skill content ships in the
local `harn` binary so it matches the version in use.

## Setup

- Run `make setup` on a fresh clone **and in each new worktree**. It installs
  hooks, optional developer tools, repo-local Node tooling when available,
  sccache config, per-worktree Cargo target config when `CODEX_WORKTREE_PATH` is
  set, and `cargo check --workspace`. A worktree created with `git worktree add`
  has no `.cargo/config.toml` until setup runs there, so it gets neither the
  shared target dir nor the compiler wrapper and every build starts cold — long
  enough that `make gen-*` and `check-*` targets hit the Cargo probe deadline and
  fail with exit 124 rather than producing anything. `scripts/harn_bin.sh` warns
  when it sees an unconfigured checkout and prints the two ways past a timeout:
  `HARN_BIN=<path> HARN_BIN_NO_BUILD=1` to reuse a binary you already built, or
  `HARN_BIN_CARGO_TIMEOUT_SECONDS=3600` to allow the cold build.
- `HARN_DEV_TARGET_WORKTREE_PATH` and `CODEX_WORKTREE_PATH` name the worktree a
  target dir belongs to. Setup ignores either one when it does not name the
  checkout being set up, and says so: a shell that exports the path of one
  checkout while you work in a sibling would otherwise give both the same
  mutable target dir.
- Build caching is two layers, written into `.cargo/config.toml` by
  `scripts/dev_setup.sh`:
  1. **Per-worktree `target-dir`** (`$TMPDIR/harn-target/<parent>-<leaf>`):
     final binaries and mutable build-script scratch stay isolated per
     worktree, while the stable path keeps repeated commands incremental.
     Do not share Cargo `build-dir` or `target-dir` paths across concurrent
     worktrees: generated `OUT_DIR` contents are mutable. Operators may set
     `HARN_DEV_BUILD_DIR=<path>` only when they deliberately provide an
     equivalent serialization boundary.
  2. **sccache** (`rustc-wrapper`): caches same-path recompiles. Immutable,
     commit-bound CI artifacts provide cross-run reuse where appropriate.
  Orphaned per-worktree target dirs are reclaimed by
  `scripts/prune_stale_targets.sh` (run from setup at most daily).
- Setup phases are fingerprinted under `.codex/dev-setup/`, so repeated setup
  is normally a fast no-op. Use `HARN_DEV_SETUP_FORCE=1 make setup` to refresh
  every phase.
- Codex app worktrees use `.codex/environments/environment.toml`, which calls
  `make setup` through the same repo bootstrap path.
- Claude Code project settings run `scripts/claude-dev-setup-once.sh` on
  session startup. It delegates to the same setup path once per dependency
  fingerprint and stores ignored logs under `.claude/dev-setup/`.
- Use `make test` for workspace Rust tests. It uses `cargo nextest` when
  available and falls back to `cargo test --workspace`.
- Use `make test-cargo` only when you need baseline Cargo behavior.
- Run `make install-hooks` if the repo hook path is not configured.
- Inspect CLI surface with `cargo run --quiet --bin harn -- --help`.
- The root `package.json` is repo tooling only. Portal, tree-sitter, and VS Code
  have their own package manifests.
- Repo-root portal scripts bootstrap `crates/harn-cli/portal/node_modules`, so
  `npm run portal:*` works in fresh worktrees.
- `crates/harn-wasm` is outside the Cargo workspace. Build it with
  `cd crates/harn-wasm && wasm-pack build`.

Two Claude Code hooks are configured in `.claude/settings.json`: session setup
(above) and a Bash guard, `scripts/claude_bash_guard.harn`. The guard rejects
`cargo build/check/clippy/fmt/test/bench` in favour of the Makefile target that
sets the right environment, and rejects piping a build or test straight into a
filter, because that throws away every line the filter did not print and the
only way to get one back is to run the whole thing again. Redirect to a file
first, then grep the file. `HARN_ALLOW_RAW_CARGO=1` in the command is the escape
hatch for a genuine one-off.

Keep installed hooks on. The default pre-commit hook runs cheap staged guards
and a read-only Rust format check; the default pre-push hook enforces signed
commits, merge-queue safety, and cheap drift guards. Required CI owns compiling,
testing, Harn formatting/linting, and generated-artifact validation. Set
`HARN_HOOKS_FULL_LOCAL=1` to opt into the targeted build-backed local gates; add
`HARN_PREPUSH_FULL_TESTS=1` to run the broader `make test` pre-push gate too.

## Repository map

- `crates/harn-lexer`: tokenizer and spans.
- `crates/harn-parser`: AST, parser, and type checker.
- `crates/harn-stdlib`: embedded Harn stdlib source catalog.
- `crates/harn-vm`: compiler, VM, stdlib, providers, orchestration, transcripts,
  bridge, and ACP integration.
- `crates/harn-cli`: CLI, conformance runner, portal server, MCP/OAuth, A2A/ACP,
  replay, and eval tooling.
- `crates/harn-lint`, `crates/harn-fmt`: linting and formatting.
- `crates/harn-lsp`, `crates/harn-dap`: editor and debugger integrations.
- `crates/harn-cli/portal/`: React/Vite persisted-run UI.
- `conformance/tests/`: executable language/runtime spec.
- `spec/chapters/*.md`: canonical language spec, one file per section
  (`spec/HARN_SPEC.md` is the generated single-file assembly).
- `docs/src/`: Markdown docs (Diataxis IA), rendered by the `website/` site.
- `website/`: Vite + React + Tailwind site for harnlang.com; builds to `docs/dist/`.
- `tree-sitter-harn/`: tree-sitter grammar and tests.
- `editors/vscode/`: VS Code extension.

## Prompt templates

The `.harn.prompt` / `.prompt` engine used by `render(...)`,
`render_prompt(...)`, and `template.render` lives in
`crates/harn-vm/src/stdlib/template.rs`. Do not add another parser or evaluator.
Host-call and script-call rendering both go through `render_template_result`.

Docs: `docs/src/prompt-templating.md` and `docs/llm/harn-quickref.md`.

Back-compat: pre-v2 `{{name}}` missing-identifier passthrough stays
byte-for-byte compatible. New constructs (`if`/`elif`/`else`, `for`, `include`,
filters, `{{# #}}`, `{{ raw }}`, `{{- -}}`) raise parse errors.

## Common commands

- Build: `cargo build`
- Run: `cargo run --bin harn -- run examples/hello.harn`
- Type-check: `cargo run --bin harn -- check <path>`
- Lint: `cargo run --bin harn -- lint <path>`
- Fix lint where supported: `cargo run --bin harn -- lint --fix <path>`
- Format check: `cargo run --bin harn -- fmt --check <path>`
- Workspace tests: `make test`
- Conformance: `cargo run --bin harn -- test conformance`
- Targeted conformance: `cargo run --bin harn -- test conformance --filter <name>`
- Full gate: `make all`
- Portal: `cargo run --bin harn -- portal`
- Portal dev loop: `npm run portal:dev`

## Verification

- Start with the narrowest check that covers the touched behavior.
- Before declaring a change clean, run `make check-drift` (a seconds-scale
  preflight of every source-reading drift/manifest guard, derived from
  `scripts/generated_artifacts.toml`) and confirm `git status` is empty. After a
  Rust-registry edit, also rebuild and run `make check-drift-binary` (the
  binary-semantics guards, which false-pass on a stale binary). These are the fast
  local subset; `make all` is still the full gate.
- Before merge, prefer `make all`.
- Syntax, parser, or keyword changes need conformance coverage plus
  `make conformance`, `make lint-harn`, `make fmt-harn`, and tree-sitter tests.
- Docs code blocks under `docs/src/` need `make check-docs-snippets`.
- Builtin or keyword changes need `make gen-highlight`.
- Portal changes need `npm run portal:lint`, `npm run portal:test`, and
  `npm run portal:build`.
- VS Code changes need `(cd editors/vscode && npm run compile)`.
- Tree-sitter changes need `(cd tree-sitter-harn && npm test)`.

Do not add `std::thread::sleep`, `tokio::time::sleep`, `Instant::now()`
polling loops, `SystemTime::now()`, or short `recv_timeout` calls to tests. Use
`tokio::time::pause()`/`advance()`, `EventLog::subscribe()`, or
`OrchestratorHarness`. See `docs/src/dev/testing.md`.

## Generated files

- Edit the per-chapter sources in `spec/chapters/*.md`, not the generated
  `spec/HARN_SPEC.md` (single-file assembly) or `docs/src/language-spec.md`
  (docs mirror); regenerate both with `make sync-language-spec`.
- Do not hand-edit `docs/theme/harn-keywords.js`; regenerate with
  `make gen-highlight`.
- Do not hand-edit `spec/protocol-artifacts/*` except `*_test.go`. Regenerate
  with `make gen-protocol-artifacts`, verify with `make check-protocol-artifacts`,
  and exercise bindings with `make check-bindings`.
- Generated or local-only paths include `docs/dist/`, `.harn-runs/`, `.harn/`,
  `.harn/receipts/`, `.claude/`, `.burin/`, `target/`, and `node_modules/`.
- `scripts/generated_artifacts.toml` is the single source of truth for every
  gen/check drift pair. Adding a generated artifact means adding a `gen-*`/
  `check-*` Makefile target *and* a registry entry; `make
  check-generated-registry` fails until the registry, the `all:` recipe, and
  the CI workflows agree, so a new drift guard can't silently skip CI. See the
  registry file header for the checklist.
- Every `check-*` target must also be classified in the registry's
  `[preflight.dispatch]` table (`source` reads committed files; `binary`
  depends on current generator/parser/checker/linter semantics; `excluded`
  needs a build/toolchain).
  `make check-generated-registry` fails until it is, so `make check-drift` /
  `check-drift-binary` (whose members derive from that table) can never silently
  omit a new guard.

## Cross-surface changes

- Syntax changes usually touch lexer, parser, spec, tree-sitter, and
  conformance fixtures.
- Runtime or builtin changes usually touch `harn-vm`, `harn-cli`, docs,
  `README.md`, `CHANGELOG.md`, and conformance fixtures.
- Keep stdlib registration authoritative. Linter and editor builtin awareness
  derives from the live stdlib.
- Public functions under `crates/harn-stdlib/src/stdlib/` must declare explicit
  return types. Use named closed records for finite shapes, `Result<T, E>` for
  fallible operations, typed maps for open-key data, and avoid papering over
  missing contracts with `any` or open `dict`.
- Register new stdlib builtins with `#[harn_builtin]` (see
  [CONTRIBUTING.md](CONTRIBUTING.md#adding-a-stdlib-builtin)). The legacy
  `SyncBuiltin` / `AsyncBuiltin` / `BuiltinGroup` / `register_builtin_group`
  DSL was removed in PR #2575; every builtin now flows through
  `#[harn_builtin]` + the workspace-global `ALL_BUILTIN_DEFS` linkme slice.
- Public CLI, builtin, or host-capability changes need user-facing docs and
  help text.
- Prompt-template syntax changes also require
  `editors/vscode/syntaxes/harn-prompt.tmLanguage.json`,
  `conformance/tests/template_*`, and `CHANGELOG.md`.

## Trust boundary

Harn owns orchestration, transcript lifecycle, replay/eval, delegated worker
lineage, and mutation session audit metadata. Hosts own approval UX, concrete
file mutations, and undo/redo semantics. For autonomous or background edits,
prefer worktree-backed execution over ambient cwd state.

## Editing source

Use the simplest editing mechanism that safely fits the change. Prefer
`std/edit` when structural addressing, cross-file rename semantics, or a
staged multi-operation preview materially reduces risk. Ordinary repository
maintenance may use normal patch tools; do not build a temporary Harn driver
solely to edit a file. The available structural and hash-guarded primitives
are documented in
[Precise edits with AST tools](docs/src/cookbook.md#precise-edits-with-ast-tools).

## Changelog fragments

- Non-trivial PRs drop a single fragment file: `changelog.d/<id>.<category>.md`.
  `<id>` is the PR or issue number, `<category>` is one of `breaking`,
  `added`, `changed`, `deprecated`, `removed`, `fixed`, `security`. The body
  is the bullet(s) you would have hand-edited under `### Heading` inside
  `## Unreleased`.
- See `changelog.d/README.md` for the format and examples.
- At release time the fragments are assembled into `## Unreleased` and the
  fragment files are deleted in the same release commit.
- The `Changelog fragment` job in the `PR gates` GitHub Actions workflow is
  the soft gate. It is diff-driven and passes on its own when a PR only
  touches docs/test/CI paths, so don't add a label preemptively. Add the
  `no-changelog-needed` label only when the gate fires on a PR that
  genuinely needs no entry (typos, internal refactors, dep bumps with no
  user-visible effect); the gate treats that as a pass. Direct edits to
  `CHANGELOG.md` are also accepted (legacy path).
- Architecture: the assembler and the per-major archive helper live in
  `harn-bump-fleet/lib/changelog.harn`; release-time invocation lives in
  `harn-bump-fleet/release_harn.harn::apply_draft_release_notes`. Bump
  helpers and tests are versioned with the harn-bump-fleet repo, not here.

## Release

- Default live release from a `harn-bump-fleet` checkout:
  `harn run --no-sandbox release_harn.harn -- --repo <harn-checkout> --mode ship-pr --agent --yes-live-release`
- The release harness prepares, commits, pushes, tags, opens the PR, and enables
  auto-merge. It pushes the signed `vX.Y.Z` tag at the pinned release commit
  before the PR merges; the tag push drives publishing and binary builds.
- Do not run `./scripts/release_ship.sh --prepare` directly for normal releases;
  it is an implementation detail of `release_harn.harn`.
- Recovery helpers: `./scripts/release_ship.sh --finalize`,
  `./scripts/release_ship.sh --bump patch`, and
  `./scripts/release_gate.sh <audit|prepare|publish|notes|full> ...`.
- Dry-run release gate:
  `./scripts/release_gate.sh full --bump patch --dry-run`
- Crate publishing dry run: `./scripts/publish.sh --dry-run`
