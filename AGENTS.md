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

- Run `make setup` on a fresh clone. It installs hooks, optional developer
  tools, repo-local Node tooling when available, sccache config, per-worktree
  Cargo target config when `CODEX_WORKTREE_PATH` is set, and
  `cargo check --workspace`.
- Build caching is three layers, written into `.cargo/config.toml` by
  `scripts/dev_setup.sh`:
  1. **Shared `build-dir`** (`$TMPDIR/cargo-build-shared`, one per machine —
     `$TMPDIR` is per-user on macOS): Cargo 1.91+ splits intermediate
     artifacts from final ones, and Cargo's own fingerprinting dedupes
     registry-dep and build-script compilation across every worktree that
     shares the path. This is what gives cross-worktree caching. Older cargos
     ignore the key harmlessly. Override with `HARN_DEV_BUILD_DIR=<path>` at
     setup time, or escape-hatch a single invocation back to an isolated
     build dir with `CARGO_BUILD_BUILD_DIR=target/build`.
  2. **Per-worktree `target-dir`** (`$TMPDIR/harn-target/<parent>-<leaf>`):
     final binaries stay isolated per worktree, so concurrent sessions never
     swap the binary under test out from under each other.
  3. **sccache** (`rustc-wrapper`): caches same-path recompiles only. Its
     Rust hash is target-dir-path-dependent, so it does NOT dedupe
     compilation across different target dirs — the shared build-dir is what
     does.
  Orphaned per-worktree target dirs are reclaimed by
  `scripts/prune_stale_targets.sh` (run from setup at most daily); it never
  touches `cargo-build-shared`.
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

Keep installed hooks on. Pre-commit runs format/lint checks and generated-file
fixups. Pre-push runs targeted package checks, generated-file drift checks, and
affected Harn format/lint checks. Set `HARN_PREPUSH_FULL_TESTS=1` to run the
broader `make test` gate before pushing.

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

Mutate source files through `std/edit` rather than freeform text patches:
`edit_apply_node` to replace a node, `edit_insert_at_anchor` to add a
sibling/child, `edit_rename_symbol` for cross-file renames, `edit_dry_run`
to preview a multi-op plan, and `edit_safe_text_patch` only when the
language has no tree-sitter grammar or the change is purely textual. The
decision tree (and a `system_reminder` snippet that nudges agent loops the
same way) lives in
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
