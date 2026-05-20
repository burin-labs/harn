# AGENTS.md

This repository implements Harn, a programming language and runtime for orchestrating AI agents.

## For agents writing Harn scripts

Before writing or editing `.harn` code, read the one-page Harn quickref. It covers syntax,
concurrency primitives (`parallel each` / `parallel settle` with `max_concurrent`), the
`llm_call` options table (including `schema_retries` + `provider: "auto"`), and the gotchas
that repeatedly trip up first-time scripters.

For new scripts, default to the explicit entrypoint form
`fn main(harness: Harness) { ... }` and route capability access through
`harness.*` (for example `harness.stdio.println("hi")`).

- In-repo: `docs/llm/harn-quickref.md`
- Trigger/orchestrator add-on: `docs/llm/harn-triggers-quickref.md`
- Canonical URL: <https://harnlang.com/docs/llm/harn-quickref.html>
- Trigger/orchestrator URL: <https://harnlang.com/docs/llm/harn-triggers-quickref.html>

Claude Code users get these autoloaded via the `harn-scripting` skill at
`.claude/skills/harn-scripting/SKILL.md`.

## Dev environment tips

- Run `make setup` on a fresh clone. It configures `.githooks/`, installs `cargo-nextest`,
  `sccache`, and `actionlint` when their toolchains are available, installs repo-local Node
  tooling including the portal frontend when `npm` is available, enables the sccache rustc wrapper,
  writes a per-worktree temp Cargo `target-dir` when `CODEX_WORKTREE_PATH` is set, and runs
  `cargo check --workspace`.
- Use `make test` for workspace Rust tests. It runs `cargo nextest` when available and falls back
  to `cargo test --workspace`. Use `make test-cargo` when you explicitly need baseline Cargo
  behavior.
- Run `make install-hooks` if the git hooks path is not already set.
- Use `cargo run --quiet --bin harn -- --help` to inspect the current CLI surface.
- The root `package.json` is only for repo tooling. The portal UI, tree-sitter grammar, and VS Code
  extension each have their own package manifests.
- The repo-root portal scripts self-bootstrap `crates/harn-cli/portal/node_modules` when needed, so
  `npm run portal:lint`, `portal:test`, `portal:build`, and the git hooks should not fail just
  because portal deps have not been installed in a fresh worktree yet.
- `crates/harn-wasm` is excluded from the Cargo workspace. Build it separately with
  `cd crates/harn-wasm && wasm-pack build`.
- Installed hooks are worth keeping on: pre-commit runs `cargo fmt`, clippy, markdown lint,
  actionlint, and portal lint; pre-push runs targeted package checks plus markdown, actionlint,
  portal, generated-file drift checks, and affected Harn format/lint checks. Set
  `HARN_PREPUSH_FULL_TESTS=1` for the broader `make test` gate before pushing.

## Repository map

- `crates/harn-lexer`: tokenizer and span tracking.
- `crates/harn-parser`: AST, parser, and type checker.
- `crates/harn-stdlib`: canonical embedded Harn stdlib source catalog.
- `crates/harn-vm`: compiler, VM, stdlib, LLM/providers, orchestration runtime, transcripts, and
  bridge/ACP integration.
- `crates/harn-cli`: `harn` CLI, conformance runner, portal server, MCP/OAuth commands, A2A/ACP
  servers, and replay/eval tooling.
- `crates/harn-lint` and `crates/harn-fmt`: linting and formatting.
- `crates/harn-lsp` and `crates/harn-dap`: editor and debugger integrations.
- `crates/harn-cli/portal/`: React/Vite UI for persisted run records.
- `conformance/tests/`: executable language/runtime spec as paired `.harn` + `.expected` files, plus
  `.error` files for intentional failures.
- `spec/HARN_SPEC.md`: canonical language spec.
- `docs/src/`: mdBook sources. `docs/src/language-spec.md` is generated from `spec/HARN_SPEC.md`.
- `docs/theme/harn-keywords.js`: generated highlight keyword list from the live lexer + stdlib.
- `tree-sitter-harn/`: tree-sitter grammar and tests.
- `editors/vscode/`: VS Code extension.

## Prompt template engine

- The `.harn.prompt` / `.prompt` template language used by `render(...)`,
  `render_prompt(...)`, and the `template.render` host capability lives in one file:
  `crates/harn-vm/src/stdlib/template.rs`. Do not add a second parser or evaluator; both
  host-call and script-call paths route through `render_template_result` in that module and
  must stay behavior-identical.
- Full reference: `docs/src/prompt-templating.md`. Condensed quickref: the "Prompt templates"
  section of `docs/llm/harn-quickref.md`.
- Back-compat contract: pre-v2 `{{name}}` silent passthrough on a missing bare identifier is
  preserved. Existing templates render byte-for-byte identically. Only the new constructs
  (`if`/`elif`/`else`, `for`, `include`, filters, `{{# #}}`, `{{ raw }}`, `{{- -}}`) raise
  parse errors.

## Core commands

- Build: `cargo build`
- Run a Harn program: `cargo run --bin harn -- run examples/hello.harn`
- Type-check: `cargo run --bin harn -- check <path>`
- Lint: `cargo run --bin harn -- lint <path>`
- Auto-fix lint where supported: `cargo run --bin harn -- lint --fix <path>`
- Check formatting: `cargo run --bin harn -- fmt --check <path>`
- Workspace tests: `make test`
- Explicit Cargo fallback: `make test-cargo`
- GitHub Actions workflow lint: `make lint-actions`
- Conformance suite: `cargo run --bin harn -- test conformance`
- Targeted conformance case: `cargo run --bin harn -- test conformance --filter <name>`
- Full repo gate: `make all`
- Portal server: `cargo run --bin harn -- portal`
- Portal full dev loop: `npm run portal:dev`

## Testing instructions

- Before merging, prefer `make all`. It runs formatting, clippy, Rust tests, conformance, markdown
  lint, Harn lint/format checks, highlight drift checks, and docs snippet parsing.
- For small changes, run the narrowest checks that cover the touched area first, then expand.
- If you change Harn syntax, parser behavior, or keywords, add or update conformance coverage and run
  `make conformance`, `make lint-harn`, `make fmt-harn`, and the relevant tree-sitter tests.
- If you change docs code blocks under `docs/src/`, run `make check-docs-snippets`.
- If you change builtins or keyword sets, run `make gen-highlight` and commit the updated
  `docs/theme/harn-keywords.js`. The pre-commit hook regenerates and re-stages this file
  automatically as a consistency fixup.
- If you change the portal frontend, run `npm run portal:lint`, `npm run portal:test`, and
  `npm run portal:build`.
- If you change the VS Code extension, run `(cd editors/vscode && npm run compile)`.
- If you change tree-sitter grammar or queries, run `(cd tree-sitter-harn && npm test)`.
- Do not add `std::thread::sleep`, `tokio::time::sleep`, `Instant::now()` polling loops,
  `SystemTime::now()`, or short `recv_timeout` calls to test files. These patterns are banned
  by `make lint-test-patterns`. Use `tokio::time::pause()`/`advance()`, `EventLog::subscribe()`,
  or `OrchestratorHarness` instead. See `docs/src/dev/testing.md` for approved patterns and the
  opt-out procedure.

## Generated files and sync rules

- Edit `spec/HARN_SPEC.md`, not `docs/src/language-spec.md`; regenerate with
  `make sync-language-spec` (which runs `scripts/sync_language_spec.harn`).
- Do not hand-edit `docs/theme/harn-keywords.js`; regenerate it with `make gen-highlight`.
- Do not hand-edit `spec/protocol-artifacts/*` (excluding `*_test.go`); regenerate Harn
  protocol contracts with `make gen-protocol-artifacts`, verify drift with
  `make check-protocol-artifacts`, and exercise the Python and Go bindings with
  `make check-bindings`. Python lives in `spec/protocol-artifacts/python/`,
  Go in `spec/protocol-artifacts/go/harnprotocol/`. The Go test file
  `harnprotocol_test.go` is hand-written and round-trips the published
  fixture.
- `docs/dist/`, `.harn-runs/`, `.harn/`, `.harn/receipts/`, `.claude/`, `.burin/`, `target/`,
  and `node_modules/` are generated or local-only paths.

## Change alignment rules

- Syntax changes usually require coordinated updates to the lexer, parser, spec, tree-sitter, and
  conformance tests.
- Runtime or builtin changes usually require coordinated updates to `harn-vm`, `harn-cli`, docs,
  `README.md`, `CHANGELOG.md`, and conformance tests.
- Keep stdlib registration authoritative. Linter and editor builtin awareness is derived from the live
  stdlib instead of a separate hardcoded list.
- When public CLI commands, builtins, or host capability behavior changes, update the user-facing docs
  and help text along with the implementation.
- Conformance tests are the main executable spec for user-visible language and runtime behavior.
- Changes to `.harn.prompt` template syntax require coordinated updates to
  `crates/harn-vm/src/stdlib/template.rs`, `docs/src/prompt-templating.md`,
  `docs/llm/harn-quickref.md`, the VS Code grammar at
  `editors/vscode/syntaxes/harn-prompt.tmLanguage.json`, conformance fixtures under
  `conformance/tests/template_*`, and `CHANGELOG.md`.

## Trust boundary

- Harn owns orchestration, transcript lifecycle, replay/eval, delegated worker lineage, and mutation
  session audit metadata.
- Hosts own approval UX, concrete file mutations, and undo/redo semantics.
- For autonomous or background edits, prefer worktree-backed execution over ambient cwd state.

## Release workflow

- Open version-bump PR after release content lands: `./scripts/release_ship.sh --bump patch`
- Finalize after bump PR lands: `./scripts/release_ship.sh --finalize`
- Audit: `./scripts/release_gate.sh audit` (uses `make test`, so `cargo-nextest`
  accelerates Rust tests when installed)
- Dry-run release gate: `./scripts/release_gate.sh full --bump patch --dry-run`
- Crate publishing helper: `./scripts/publish.sh --dry-run`
- `release_ship.sh --finalize` pushes the tag before `cargo publish` so
  downstream consumers (e.g. `burin-code`'s `fetch-harn`, GitHub release
  binary workflows) run in parallel with crates.io publication.
