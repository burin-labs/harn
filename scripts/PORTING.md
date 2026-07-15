# Porting repo scripts to Harn

This repo is dogfooding Harn: maintenance/check/codegen scripts that are pure
logic (read files, regex, validate, emit diagnostics, exit code) are being
cut over from Python/Bash to `.harn`. This file tracks what has moved, what is
intentionally staying in another language.

It is a living tracker, not a spec. When you port a script, move its row to
**Ported** and delete the original; when you add a new check, add it as a
`.harn` script directly.

## Ported

| Script | Wired into | Notes |
| --- | --- | --- |
| `check_receipt_struct_duplication.harn` | `make check-receipt-structs` | Workspace `.rs` scan + brace matching. |
| `sync_tree_sitter_keywords.harn` | `make {gen,check}-tree-sitter-keywords` | Lexer↔grammar keyword set sync (`--write`). |
| `check_diagnostic_codes.harn` | `make lint-diagnostic-codes` | Registry + struct-literal/`Code::` scan; uses `regex_captures` `.line`/`.start`. |
| `check_docs_links.harn` | `make check-docs-links` | Markdown local-link resolver; `..` resolved at `fs.exists` time. |
| `verify_language_spec.harn` | `scripts/release_gate.sh` (`verify_language_spec` phase) | Extracts ```harn fences from the spec and type-checks each; resolves the checker binary via `cargo metadata`. |
| `verify_release_metadata.harn` | `ci.yml` + `release_gate.sh` | Cargo.toml↔CHANGELOG checks; reuses `std/semver`; imports `render_release_notes.harn` in-process for its render smoke check. |
| `render_release_notes.harn` | `release_gate.sh` + `build-release-binaries.yml` | CHANGELOG section → GitHub notes; the release-asset job invokes the built linux-x64 release binary (no toolchain there). |
| `sync_protocol_fixture_runtime_versions.harn` | `scripts/release_gate.sh` | Fixture runtime-version bump; `--write` produces byte-identical fixtures. |
| `build_release_assets_manifest.harn` | `release_gate.sh` + `build-release-binaries.yml` | Release-asset manifest; byte-identical JSON vs Python `json.dumps(sort_keys)`. |
| `backfill_stdlib_metadata.harn` | manual tool | Stdlib-metadata backfill (mutating); verified `diff -r`-identical mutation + idempotent (re-run = no-op). |
| `check_vm_rss_soak.harn` | `make check-vm-rss-soak` (ci.yml) | Spawns `harn bench --profile-json`, checks tail RSS growth; analysis logic parity-tested (RSS magnitude is inherently nondeterministic). |
| `lint_test_patterns.harn` | `make lint-test-patterns` | Wall-clock/deflake test lint; whole-file `regex_captures` scans (per-line VM loops were ~50x slower); output byte-identical to the bash original on clean + injected-violation runs. |
| `check_docs_cli_flags.harn` | `make check-docs-cli-flags` | Docs bash/sh `harn` flag audit; hand-rolled POSIX-shlex tokenizer + `--help` caches threaded through state records (dicts are value-typed); parity-verified incl. parse-error/missing-flag output. |
| `check_binary_size.harn` | `build-release-binaries.yml` (invokes the just-built linux-x64 release binary) | Size budget + cargo-bloat report; parity-verified on `--no-build` pass/fail/usage/missing-bloat paths. Sandbox note: out-of-tree HARN_BIN/CARGO_TARGET_DIR needs `--no-sandbox`. |
| `check_docs_snippets.harn` | `make check-docs-snippets` + `release_gate.sh` | ```` ```harn ```` fence extraction + per-block `harn parse`; ```` ```harn,check ```` opts into full `harn check`; clean run and failure output. |
| `check_docs_workflow_quickstart.harn` | `make check-docs-workflow-quickstart` | Pinned digest / executed-node / connector-shape assertions via `spawn_captured` (+`cwd` for the connect demo); parity-verified incl. digest-drift failure. |
| `check_docs_model_refs.harn` | `make check-docs-model-refs` + `release_gate.sh` | `toml_parse` of `aliases.sonnet` replaces the awk section scan; `regex_captures` line numbers replace `rg -n -o`. |
| `check_site_snippets.harn` | `make check-site-snippets` | Site + demo-gallery snippet `harn check`; child stdout/stderr streamed through. |
| `check_changelog_no_retroactive_edits.harn` | `.github/workflows/ci.yml` + pre-push hook | Published-version CHANGELOG guard; bypasses require env/trailer review signal. |
| `check_rust_prompt_prose.harn` | `scripts/check_no_rust_prompt_prose.sh` + pre-commit hook | Rust prompt-prose ratchet with stable allowlist/digest contract. |
| `check_generated_registry.harn` | `make check-generated-registry` + pre-push hook | Registry/Makefile/workflow generated-artifact guard. |
| `check_python_boundary.harn` | `make check-python-boundary` + audit gate | Ratchets Python usage to explicit bootstrap/platform/generated/fixture reasons. |
| `affected-crates.harn` | `make test-affected` + `ci.yml` Rust lanes | Canonical PR fast-feedback crate selector. The CI bootstrap path mirrors it in `scripts/ci/affected_crate_args.sh` so package selection can happen before compiling the Harn CLI. |
| `bench_cli_cold_start.harn` | `scripts/bench_cli_cold_start.sh` + `cli-cold-start-budget.yml` | Cold-start benchmark controller; isolates measured Harn subprocesses with `env -i` so controller runtime state cannot leak into the timed child. |
| `verify_tree_sitter_parse.harn` | `release_gate.sh` grammar audit | Regenerates/builds the tree-sitter grammar when stale, sweeps positive `.harn` sources, and preserves strict/non-strict failure policy. |
| `check_burin_protocol_bindings.harn` | `make check-burin-protocol-artifacts` | Cross-repo Swift/TypeScript protocol artifact drift check against a Burin checkout. |
| `check_protocol_bindings.harn` | `make check-bindings` | Harn owns the round-trip driver and diagnostics; Python is invoked only as the generated Python SDK runtime under test. |
| `verify_crate_packages_plan.harn` | `scripts/verify_crate_packages.sh` | Cargo metadata normalization for package verification; the shell remains the Cargo/tar driver and Python remains only for Rust include-path inspection. |

Each ported script has a paired `scripts/tests/<name>_test.harn` exercising its
pure helpers, run by `make test-harn-scripts`.

## Kept in Python — toolchain reason

There are no repo-owned Python orchestration scripts left in this category. New
Python requires a concrete foreign-toolchain, generated-binding, external-spec,
or subprocess-fixture reason and must be allowlisted in
`check_python_boundary.harn`.

## Temporary Harn cutover debt

There is no active repo-owned Python cutover debt. New Python requires a
concrete foreign-toolchain, generated-binding, external-spec, or subprocess
fixture reason and must be allowlisted in `check_python_boundary.harn`.

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
- **`spec/protocol-artifacts/*.ts`, `python/harn_protocol.py`,
  `harn-protocol.ts`** — generated multi-language SDK bindings for external
  consumers.
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

### Known Harn limitations surfaced by the ports

Not bugs — documented so future ports plan around them:

- **Regex backreferences are unsupported** (the VM uses the Rust `regex`
  crate). A pattern like `(['"])(.*?)\1` throws "Invalid regex". Rewrite with
  explicit alternation (e.g. `"([^"]*)"|'([^']*)'` and read whichever group is
  non-nil), as `check_docs_links.harn` does for HTML `href`/`src`.
- **Regex look-around is unsupported** (look-ahead `(?=...)`/`(?!...)`,
  look-behind). This is a deliberate linear-time guarantee (RE2/ripgrep-style),
  not a defect — see the regex-engine note below. Section extraction
  (`(?=^## )`) and whole-token matching (`(?![\w-])`) are reimplemented with
  small line/char scanners in `check_generated_registry.harn`,
  `verify_release_metadata.harn`, and `render_release_notes.harn`.
- **String literals don't recognize `\u{NN}`, `\xNN`, `\f`, `\v`** — they
  produce multi-char literals (and `harn fmt` escapes the backslash). Match
  whitespace via `ch.trim() == ""` (Rust `char::is_whitespace`, same set as
  Python `str.isspace()` for source-legal bytes) rather than escape sequences.
- **List slicing is `.slice(start, end)`** (not `.substring`, which is
  string-only).

### Round-3 notes (bash `check_*.sh` cutover)

Surfaced by the batch-3 ports (`lint_test_patterns`, `check_docs_cli_flags`,
`check_binary_size`, `check_docs_snippets`, `check_docs_workflow_quickstart`,
`check_docs_model_refs`, `check_site_snippets`):

- **Per-line VM scanning is ~50x slower than whole-file regex.** A
  closure-per-line `contains` loop over the 750 kLOC test corpus ran >2 min;
  one `regex_captures(pattern, whole_file, "m")` per pattern per file (then
  deduping on `m.line`, like `grep -n`) brought `lint_test_patterns` to ~8s —
  faster than the bash original (~22s).
- **Multiline scans can misattribute line numbers.** In a whole-file scan, a
  grep ERE like `(^|[^_a-zA-Z])sleep\(` lets the negated class match the
  *previous line's trailing newline*, so `m.line` points one line up. Exclude
  `\n` from negated classes (`[^_a-zA-Z\n]`) when porting line-based greps.
- **Block comments nest.** A `/** ... */` doc comment containing a glob like
  `crates/**` ends up with a `/**` opener inside it and eats the rest of the
  file ("Unterminated block comment"). Avoid `/*` sequences in doc prose.
- **`harness.fs.glob` matches directories too.** `find -type f` ports need a
  `harness.fs.stat(p).is_file` gate (a directory literally named `.harn`
  matches `**/*.harn` via an empty `*`).
- **Dicts/lists are value-typed across `fn` boundaries**, so memo caches
  (e.g. `--help` output keyed by subcommand path) must be threaded through
  helper returns (`{state, value}`) rather than mutated in place.
- **The `harn run` sandbox scopes fs builtins to the workspace root.**
  Scripts that may be pointed at out-of-tree paths (`check_binary_size.harn`
  with a redirected `CARGO_TARGET_DIR`) need callers to pass `--no-sandbox`;
  in-tree CI usage needs nothing.

### Round-2 stdlib groundwork (to keep complex ports DRY)

The batch-2 ports (`check_diagnostic_codes`, `check_docs_links`,
`verify_language_spec`) are behavior-identical and tested, but run ~1.8–2.8×
the LOC of their Python originals because Python leans on rich stdlib the Harn
ecosystem hasn't surfaced yet, so each port reimplements those primitives
inline. The verbosity is the signal for the next groundwork wave — landing
these would let the ports (and future ones) shrink toward parity:

- **Indexed string ops:** `str.index_of(needle, from)` and
  `str.starts_with(needle, from)` (Python's `str.find(x, i)` /
  `str.startswith(x, i)`). `check_diagnostic_codes` hand-rolls `find_from` /
  `starts_with_at`.
- **`std/path` resolution:** `resolve`/`normalize` that collapses `.`/`..`, and
  `with_suffix`/`suffix`. `check_docs_links` reimplements `pathlib.Path.resolve`
  and `.with_suffix`.
- **A `urllib.parse.unquote`-equivalent decode.** The existing `url_decode`
  builtin decodes `+`→space (i.e. `unquote_plus` semantics), so it is *not* a
  drop-in for Python's `unquote`; `check_docs_links` correctly hand-rolls the
  percent-decode. A `+`-preserving variant would let it drop the helper.

### Regex engine: linear-time by design

The VM's regex builtins wrap the Rust `regex` crate, which guarantees
linear-time matching and therefore **excludes look-around and backreferences**
(the same trade-off RE2 and ripgrep make). For a runtime that may evaluate
agent-authored patterns, that DoS-resistance is the SOTA-correct posture — not
a gap to "fix" by bolting on a backtracking engine. The cutover's actual regex
need was *positional* info, which landed in #3031 (`regex_captures` now returns
`start`/`end`/`line` and takes `flags`). The look-around/backreference patterns
are handled by small scanners, which is the right call. If a future use case
genuinely needs fancy features, the considered option is an *opt-in* fallback
(e.g. `fancy-regex`) scoped to patterns the linear engine rejects — a
deliberate decision to accept backtracking, not a default.

### Groundwork follow-ups

- **tree-sitter editor highlighting for `r#"..."#`.** The lexer is authoritative
  and the feature works end-to-end; the editor grammar (`grammar.js`) needs an
  external scanner to highlight hashed raw strings (tree-sitter's token regex
  lacks the lookahead to match the `"#` close). Tracked as editor-tooling work.
