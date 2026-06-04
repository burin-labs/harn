# Porting repo scripts to Harn

This repo is dogfooding Harn: maintenance/check/codegen scripts that are pure
logic (read files, regex, validate, emit diagnostics, exit code) are being
cut over from Python/Bash to `.harn`. This file tracks what has moved, what is
queued, and what is intentionally staying in another language.

It is a living tracker, not a spec. When you port a script, move its row to
**Ported** and delete the original; when you add a new check, add it as a
`.harn` script directly.

## Ported

| Script | Wired into | Notes |
| --- | --- | --- |
| `check_receipt_struct_duplication.harn` | `make check-receipt-structs` | Workspace `.rs` scan + brace matching. |
| `sync_tree_sitter_keywords.harn` | `make {gen,check}-tree-sitter-keywords` | Lexer↔grammar keyword set sync (`--write`). |

Each ported script has a paired `scripts/tests/<name>_test.harn` exercising its
pure helpers, run by `make test-harn-scripts`.

## Queued — portable, not yet ported

Pure-logic checks/codegen with no foreign-runtime coupling. Good Harn targets;
left for follow-up waves to keep each PR reviewable.

| Script | Why portable | Risk to watch |
| --- | --- | --- |
| `check_diagnostic_codes.py` | Rust-source regex + brace scan | Largest; uses match offsets/line (now in `regex_captures`). |
| `check_docs_links.py` | Markdown link scan + `fs.exists` | Path-candidate resolution; OS resolves `..` at stat time. |
| `verify_language_spec.py` | Spec mirror text compare | — |
| `check_generated_registry.py` | Generated-artifact registry check | — |
| `check_rust_prompt_prose.py` | Rust-source prose lint | Ratchet pattern in `.githooks/lib.sh`. |
| `verify_release_metadata.py` | Cargo metadata validation | — |
| `render_release_notes.py` | Changelog → notes render | Release path (`build-release-binaries.yml`). |
| `build_release_assets_manifest.py` | Asset manifest build | Release path. |
| `sync_protocol_fixture_runtime_versions.py` | Fixture version sync | — |
| `backfill_stdlib_metadata.py` | Metadata backfill | Mutating script; verify idempotence. |
| `check_changelog_no_retroactive_edits.py` | git + regex | High blast radius: also a `pre-push` hook. |
| `affected-crates.py` | git diff → crate set | **Critical CI path** (`ci.yml` test matrix); port last, parity-test hard. |
| `check_vm_rss_soak.py` | spawns `harn`, samples RSS | Runtime soak; needs process spawn + sampling. |
| Bash `check_*.sh` (e.g. `check_binary_size`, `check_docs_cli_flags`, `check_docs_snippets`, `lint_test_patterns`) | validation logic | Portable; lower priority than the Python checks. |

## Out of scope — stays in its current language

External-toolchain or foreign-artifact reasons; porting would defeat the
script's purpose or fight a tool that is JS/Python by design.

- **`website/**`, `crates/harn-cli/portal/**`, `editors/vscode/**`** — real
  Vite/React/VS Code apps; require the Node/TS toolchain.
- **`tree-sitter-harn/grammar.js`, `grammar/keywords.js`, `scripts/tree-sitter-cli.sh`**
  — tree-sitter grammars are authored in JS by design. (`keywords.js` is
  *generated* from the lexer by `sync_tree_sitter_keywords.harn`.)
- **`crates/harn-hostlib/tests/fixtures/ast/**` (`source.jsx`, `source.rb`,
  `source.ts`, `source.py`, …)** — AST-parser test *inputs*; must be in the
  foreign languages they exercise.
- **`spec/protocol-artifacts/*.ts`, `python/harn_protocol.py`, `harn-protocol.ts`,
  `spec/provider-catalog/harn-provider-catalog.ts`** — generated multi-language
  SDK bindings for external consumers.
- **`check_protocol_bindings.py`, `check_burin_protocol_bindings.py`,
  `verify_tree_sitter_parse.py`** — these *import the generated foreign bindings
  / compiled grammar and round-trip through them*. Porting to Harn would stop
  testing the artifact they exist to validate.
- **`conformance/helpers/*.py`** (github/slack/linear/mcp/http mock servers) —
  out-of-process mock servers for the conformance harness; intentionally foreign
  infra spawned as subprocesses.
- **`docs/theme/harn-keywords.js`** — docs-site theme JS (browser).
- **`experiments/**`, `opentrustgraph-spec/examples/*.py`** — throwaway
  experiments / external spec examples.

## Build-orchestration Bash — kept as Bash

These drive `cargo`, `git`, codesigning, and release plumbing. They are
idiomatic shell glue around external build tools; porting offers little and
risks release/build reliability:

`release_ship.sh`, `release_gate.sh`, `release_smoke.sh`, `publish.sh`,
`verify_crate_packages.sh`, `dev_setup.sh`, `sign_local_macos.sh`,
`generate_sdk_clients.sh`, `prune_stale_targets.sh`, `bench_*.sh`,
`smoke_installed_binary.sh`, `stress_subprocess_tests.sh`,
`nextest_filters_from_paths.sh`, `measure_lean_embedding.sh`,
`build_docs_site.sh`, `configure_merge_drivers.sh`, `ensure_portal_deps.sh`,
`portal_demo.sh`, `demo_local_a2a_dispatch.sh`, `install.sh`,
`.githooks/*`, `.github/scripts/*.sh`.

## Toolchain groundwork landed for this cutover

The port surfaced two ergonomics gaps; both are fixed in the language so ports
stay DRY (no LOC blow-up vs. the Python original):

1. **`regex_captures` now reports positions.** Each match record carries
   `start`/`end` (char offsets) and `line` (1-based), and the builtin accepts an
   optional `flags` arg (`"i"`, `"m"`, `"s"`, `"x"`) like `regex_match`. This is
   the equivalent of Python's `m.start()` / `text.count("\n", 0, m.start())+1`,
   which 7 of the queued scripts need for positional diagnostics.
2. **Hashed raw strings `r#"..."#`.** Raw strings can now embed literal `"`
   (Rust-style; add more `#` as needed). Quote-heavy regexes (matching string
   literals, JSON, etc.) no longer need backslash-escaped non-raw strings.

### Groundwork follow-ups

- **tree-sitter editor highlighting for `r#"..."#`.** The lexer is authoritative
  and the feature works end-to-end; the editor grammar (`grammar.js`) needs an
  external scanner to highlight hashed raw strings (tree-sitter's token regex
  lacks the lookahead to match the `"#` close). Tracked as editor-tooling work.
