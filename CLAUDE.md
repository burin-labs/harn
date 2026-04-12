# AGENTS.md

This repository implements Harn, a programming language and runtime for orchestrating AI agents.

## Dev Environment Tips

- Run `make setup` on a fresh clone. It configures `.githooks/`, installs `cargo-nextest` and
  `sccache`, installs repo-local Node tooling when `npm` is available, enables the sccache
  rustc wrapper, and runs `cargo check --workspace`.
- Use `make test` for workspace Rust tests. It runs `cargo nextest` when available and falls back
  to `cargo test --workspace`. Use `make test-cargo` when you explicitly need baseline Cargo
  behavior.
- Run `make install-hooks` if the git hooks path is not already set.
- Use `cargo run --quiet --bin harn -- --help` to inspect the current CLI surface.
- The root `package.json` is only for repo tooling. The portal UI, tree-sitter grammar, and VS Code
  extension each have their own package manifests.
- `crates/harn-wasm` is excluded from the Cargo workspace. Build it separately with
  `cd crates/harn-wasm && wasm-pack build`.
- Installed hooks are worth keeping on: pre-commit runs `cargo fmt`, clippy, markdown lint, and
  portal lint; pre-push runs workspace tests plus markdown, portal, and conformance format checks.

## Repository Map

- `crates/harn-lexer`: tokenizer and span tracking.
- `crates/harn-parser`: AST, parser, and type checker.
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

## Core Commands

- Build: `cargo build`
- Run a Harn program: `cargo run --bin harn -- run examples/hello.harn`
- Type-check: `cargo run --bin harn -- check <path>`
- Lint: `cargo run --bin harn -- lint <path>`
- Auto-fix lint where supported: `cargo run --bin harn -- lint --fix <path>`
- Check formatting: `cargo run --bin harn -- fmt --check <path>`
- Workspace tests: `make test`
- Explicit Cargo fallback: `make test-cargo`
- Conformance suite: `cargo run --bin harn -- test conformance`
- Targeted conformance case: `cargo run --bin harn -- test conformance --filter <name>`
- Full repo gate: `make all`
- Portal server: `cargo run --bin harn -- portal`
- Portal full dev loop: `npm run portal:dev`

## Testing Instructions

- Before merging, prefer `make all`. It runs formatting, clippy, Rust tests, conformance, markdown
  lint, Harn lint/format checks, highlight drift checks, and docs snippet parsing.
- For small changes, run the narrowest checks that cover the touched area first, then expand.
- If you change Harn syntax, parser behavior, or keywords, add or update conformance coverage and run
  `make conformance`, `make lint-harn`, `make fmt-harn`, and the relevant tree-sitter tests.
- If you change docs code blocks under `docs/src/`, run `make check-docs-snippets`.
- If you change builtins or keyword sets, run `make gen-highlight` and commit the updated
  `docs/theme/harn-keywords.js`.
- If you change the portal frontend, run `npm run portal:lint`, `npm run portal:test`, and
  `npm run portal:build`.
- If you change the VS Code extension, run `(cd editors/vscode && npm run compile)`.
- If you change tree-sitter grammar or queries, run `(cd tree-sitter-harn && npm test)`.

## Generated Files And Sync Rules

- Edit `spec/HARN_SPEC.md`, not `docs/src/language-spec.md`; regenerate with
  `./scripts/sync_language_spec.sh`.
- Do not hand-edit `docs/theme/harn-keywords.js`; regenerate it with `make gen-highlight`.
- `docs/dist/`, `.harn-runs/`, `.harn/`, `.claude/`, `.burin/`, `target/`, and `node_modules/` are
  generated or local-only paths.

## Change Alignment Rules

- Syntax changes usually require coordinated updates to the lexer, parser, spec, tree-sitter, and
  conformance tests.
- Runtime or builtin changes usually require coordinated updates to `harn-vm`, `harn-cli`, docs,
  `README.md`, `CHANGELOG.md`, and conformance tests.
- Keep stdlib registration authoritative. Linter and editor builtin awareness is derived from the live
  stdlib instead of a separate hardcoded list.
- When public CLI commands, builtins, or host capability behavior changes, update the user-facing docs
  and help text along with the implementation.
- Conformance tests are the main executable spec for user-visible language and runtime behavior.

## Trust Boundary

- Harn owns orchestration, transcript lifecycle, replay/eval, delegated worker lineage, and mutation
  session audit metadata.
- Hosts own approval UX, concrete file mutations, and undo/redo semantics.
- For autonomous or background edits, prefer worktree-backed execution over ambient cwd state.

## Release Workflow

- Full release from a clean content commit: `./scripts/release_ship.sh --bump patch`
- Audit: `./scripts/release_gate.sh audit` (uses `make test`, so `cargo-nextest` accelerates Rust tests when installed)
- Dry-run full release: `./scripts/release_gate.sh full --bump patch --dry-run`
- Crate publishing helper: `./scripts/publish.sh --dry-run`
- `release_ship.sh` pushes branch + tag before `cargo publish` so
  downstream consumers (e.g. `burin-code`'s `fetch-harn`, GitHub release
  binary workflows) run in parallel with crates.io publication.
