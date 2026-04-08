# Changelog

All notable changes to Harn are documented in this file.

## v0.5.45

### Added

- **`tool` keyword** — declarative tool definition syntax:
  `tool name(params) -> type { description "..."; body }`. Compiles to
  `tool_define` under the hood. Supported across parser, typechecker,
  compiler, formatter, linter, LSP, and tree-sitter grammar.
- **Together AI provider** — built-in provider configuration
  (`TOGETHER_AI_API_KEY`, `TOGETHER_AI_BASE_URL`).
- **`provider_register` builtin** — register custom provider names at
  runtime so `llm_call` can dispatch to them.
- **LLM provider trait architecture** — new `llm/provider.rs` and
  `llm/providers/` directory with trait-based provider dispatch
  (Anthropic, OpenAI-compatible, Ollama, Mock).
- **Events system** — `events.rs` module with `EventSink` trait and
  `StderrSink` default for structured observability.
- **`observed_llm_call` wrapper** — unified single-LLM-call function
  with call-ID generation, bridge notifications, retry logic, and span
  annotation. Deduplicates instrumentation previously split across
  `llm_call` and `agent_loop`.
- **OpenTelemetry support** — optional `otel` feature flag on harn-vm
  (opentelemetry, opentelemetry_sdk, opentelemetry-otlp dependencies).
- **Cross-file unused-function lint** — `harn check` and `harn lint`
  pre-scan selective imports across files so library functions consumed
  by other files are not falsely flagged as unused.
- **Per-request LLM timeout** — the `timeout` option on `llm_call`
  and `llm_completion` is now applied to HTTP requests (previously
  computed but unused).
- **`tool_examples` agent config** — optional few-shot examples
  injected into the tool-calling contract prompt.

### Changed

- **Orchestration module split** — monolithic `orchestration.rs`
  (4379 lines) refactored into `orchestration/` directory with modules:
  artifacts, compaction, hooks, policy, records, workflow.
- **Agents stdlib refactor** — workflow and run-record builtins
  extracted from `agents.rs` into `stdlib/workflow.rs` and
  `stdlib/records.rs`.
- **Better keyword error messages** — parser now reports
  `'tool' (reserved keyword)` when a keyword is used where an
  identifier is expected.
- **Env var quote stripping** — `resolve_base_url` strips surrounding
  quotes from environment variable values (common `.env` parser issue).
- **Structured warning output** — LLM parameter validation warnings
  now use the events system instead of raw `eprintln!`.

## v0.5.44

### Added

- **`unused-function` lint rule** — the linter now warns when a non-pub,
  non-method function is declared but never called. Functions prefixed with
  `_` are exempt. Includes suggestion to remove or prefix.
- **Tool loop detection** — the agent loop tracks repeated identical tool
  calls (same tool + args + result) and intervenes with increasingly forceful
  redirections: warn (append hint), block (replace result), skip (don't
  execute). Configurable via `loop_detect_warn`, `loop_detect_block`, and
  `loop_detect_skip` options (defaults: 2, 3, 4).
- **Compact tool schemas** — tools can set `compact: true` to render as
  one-liner summaries in the prompt instead of full TypeScript declarations,
  reducing token usage for well-known tools.
- **`stream` option** — `llm_call` and `agent_loop` accept `stream: false`
  to use synchronous request/response instead of SSE streaming. Default:
  `true`. Environment variable: `HARN_LLM_STREAM`.
- **Custom `nudge` in ModelPolicy** — workflow stage model policies can
  specify a custom nudge message via the `nudge` field.
- **Provider key cache** — API key availability is cached per-provider for
  the process lifetime, avoiding redundant environment variable probes.
- **Provider auto-fallback** — when the default provider (anthropic) has no
  API key, the runtime falls back to `ollama` or `local` if available.
- **Model-specific parameter defaults** — `providers.toml` `model_params`
  entries (temperature, presence_penalty, etc.) are applied automatically
  when the caller doesn't specify them.
- **Tool calls inside Markdown fences** — the text tool parser now extracts
  tool calls from inside `` ``` `` fences instead of ignoring them.
- **Angle-bracket tool call wrapping** — handles `<tool(...)>` syntax
  emitted by some models (Qwen).
- **Thinking tag stripping** — leaked `<think>`/`</think>` tags from
  Qwen/Gemma models are stripped before tool-call parsing.
- **Done sentinel requires tool use** — the `##DONE##` sentinel is only
  honoured after the model has made at least one tool call, preventing
  premature exit without action.

### Changed

- **Default `max_tokens`** — raised from 0 (provider default) to 16384 to
  prevent degenerate repetition loops while leaving headroom for reasoning.
- **Simplified nudge handling** — prose-only turn collapsing and multi-tier
  nudge messages replaced with a single concise nudge.
- **Tool-call contract prompt** — shortened the how-to-call-tools
  instructions for lower prompt overhead.

### Fixed

- **OpenRouter `enable_thinking`** — skipped for OpenRouter providers where
  it causes empty all-thinking responses.
- **OpenRouter duplicate `finish_reason`** — only the first `finish_reason`
  SSE chunk is honoured, preventing in-progress tool calls from being
  truncated.
- **Ollama tuning defaults** — `min_p=0.05`, `repeat_penalty=1.05`, and
  `num_predict` from `max_tokens` are set automatically for Ollama to
  reduce garbage tokens and unbounded generation.

### Internal

- Deduplicated HTTP verb handlers, `collect_harn_files`, crypto hash
  registration, JSON validation helpers, error category checks,
  `pad_left`/`pad_right`, and LLM fallback provider logic.

## v0.5.43

### Added

- **Prose turn collapsing** — when the agent loop detects sustained prose-only
  output (no tool calls), it now collapses prior prose messages in
  `visible_messages` to reclaim context tokens. After 3 silent continuation
  turns, subsequent nudge cycles collapse accumulated prose into compact
  markers, preventing unbounded context growth from chatty model behavior.

### Fixed

- **Native JSON fallback parser** — the text tool parser now checks for
  OpenAI-style `[{"id":"call_...","function":{...}}]` JSON when no text-format
  calls are found, catching models that emit function-calling JSON despite
  text-format instructions.
- **Ollama empty-content retry** — transient ollama errors where the server
  reports `eval_count` tokens but delivers no content (EOF/parser bugs) are now
  classified as retryable instead of fatal, preventing single-point-of-failure
  crashes in eval runs.
- **LLM retry budget** — default retries increased from 2 to 3 for better
  resilience with local models.

## v0.5.42

### Changed

- **Tool-call prompt contract cleanup** — the runtime-owned tool-calling
  contract now consistently tells models to use heredoc syntax for multiline
  code/content and explicitly warns that backtick template literals break when
  the payload itself contains backticks. This removes contradictory guidance
  that was nudging models toward malformed calls.

### Fixed

- **Auto-compaction threshold accounting** — `agent_loop` now includes the
  system prompt/tool-contract size when estimating transcript tokens for
  auto-compaction, so compaction triggers before the real context window is
  exceeded instead of undercounting large prompts.
- **Malformed tool-call recovery guidance** — parse-error recovery now shows a
  concrete heredoc example instead of suggesting backtick escaping, which
  better matches the parser and reduces repeated malformed retries.

## v0.5.41

### Added

- **Comprehensive stdlib unit tests** — added 85 new unit tests across
  four previously untested modules:
  - `crypto` (22 tests): base64 round-trip, URL encode/decode edge cases,
    SHA-256/224/384/512/512-256 known vectors, MD5, hash_value determinism.
  - `regex` (16 tests): match/replace/captures, named groups, optional
    groups, Unicode, cache eviction, invalid regex errors.
  - `concurrency` (15 tests): atomics (get/set/add/CAS), circuit breaker
    state machine (open/close/reset/half-open), timer lifecycle.
  - `json` (19 tests): schema_extend/partial/pick/omit, recursive nested
    partial, json_extract from code fences and balanced structures,
    find_balanced_json with escapes and unicode.
- **Agent loop unit tests** (13 tests): `extract_retry_after_ms` edge cases
  (fractional seconds, case-insensitive, non-numeric, non-string errors),
  `is_read_only_tool` allowlist coverage.
- **Diff algorithm unit tests** (14 tests): empty/identical/insert/delete/
  all-changed/large-similar inputs, Myers primitives, header and path
  formatting.

### Changed

- **O(nd) Myers diff algorithm** — replaced the O(mn) LCS table in
  `render_unified_diff` with Myers' shortest-edit-script algorithm.
  Time complexity is now O(nd) where d = edit distance; space is O(d*n)
  instead of O(m*n). For similar files (small d), this is dramatically
  faster and avoids the unbounded memory allocation that could OOM on
  large artifact diffs.

### Fixed

- **Conformance: `agent_runtime_features`** — updated stale assertion that
  expected mock LLM output from compaction; auto-compact now uses
  observation masking and the test matches accordingly.

## v0.5.40

### Fixed

- **DRY: transcript validation in conversation builtins** — extracted
  `require_transcript()` helper, eliminating ~120 lines of duplicated
  match-and-error boilerplate across 14 transcript builtins.
- **DRY: `normalize_run_record` duplication** — now delegates to
  `parse_json_payload()` instead of inlining the same deserialize-with-path
  pattern and snippet truncation logic.
- **DRY: transcript string field helpers** — `transcript_summary_text` and
  `transcript_id` now share a `transcript_string_field` helper.
- **Performance: O(n²) stage lookup in `diff_run_records`** — replaced
  linear `.find()` scans with pre-built `BTreeMap` indices.
- **Performance: O(n²) stage lookup in `evaluate_run_against_fixture`** —
  same index-based fix.

### Added

- Conformance tests: `select_only_default`, `typed_catch_variants`,
  `finally_nested_return` (310 total).

## v0.5.39

### Added

- **Tool-call repair micro-executor** — when the text-based tool-call parser
  fails to extract any valid calls from a model response, a lightweight LLM
  call now attempts to recover the intended tool calls as structured JSON.
  This replaces the expensive nudge→retry loop with a single cheap extraction
  call, improving reliability for models that emit slightly malformed
  invocations.
- **Configurable `max_iterations` / `max_nudges` in `ModelPolicy`** — workflow
  stage nodes can now override the default agent-loop iteration cap (16) and
  consecutive-text-only nudge limit (3) via `model_policy`.

### Changed

- **Tool-call prefix stripping** — the text tool-call parser now strips
  common model-generated prefixes (`call:`, `tool:`, `use:`) before matching
  tool names, improving parse success for models that prepend these tokens.
- **Tool result format** — agent-loop tool results now use
  `[result of name]...[end of name result]` bracket notation instead of
  XML-style `<tool_result>` tags, reducing accidental XML nesting issues.
- **Default `max_tokens` behaviour** — `max_tokens` now defaults to "omit
  from request" (0) instead of a hardcoded 16384, letting providers use their
  own output limits. Anthropic-style APIs still fall back to 8192 as required
  by that API.
- **Contract prompt improvements** — clearer formatting rules, explicit
  instruction that every response must include a tool call, and guidance to
  batch independent calls.
- **Capability ceiling simplified** — `builtin_ceiling()` now returns empty
  capabilities/tools, deferring entirely to the host capability manifest
  instead of maintaining a stale allowlist that could silently block
  host-added capabilities.

### Fixed

- **UTF-8 char-boundary panic in `ThinkingStreamSplitter`** — the carry/split
  logic now floors to the nearest char boundary, preventing panics on
  multi-byte codepoints (e.g. em-dash) in streamed thinking blocks.

## v0.5.38

### Added

- **Tool parser and schema-renderer regression coverage** — added
  `crates/harn-vm/src/llm/tools_tests.rs` for the fenceless TypeScript-style
  tool-call parser and JSON-Schema-to-TypeScript contract renderer, plus new
  conformance tests for dotted closure calls, frame-scoped iterator returns,
  and workflow tool-handler preservation.
- **String case conversion builtins** — `snake_to_camel`, `snake_to_pascal`,
  `camel_to_snake`, `pascal_to_snake`, `kebab_to_camel`, `camel_to_kebab`,
  `snake_to_kebab`, `kebab_to_snake`, `pascal_to_camel`, `camel_to_pascal`,
  `title_case`, `uppercase_first`, `lowercase_first`. `camel_to_snake`
  uses the common acronym convention so `"HTTPServer"` → `"http_server"`.
  Replaces ~56 lines of per-project snake↔camel aliasing in downstream
  consumers with a single stdlib call.
- **`dict.rekey(fn)` / `dict.map_keys(fn)` method** — returns a new dict
  with each key replaced by `fn(old_key)`; last write wins on collision.
  Composes with the new case converters: `snake_dict.rekey(snake_to_camel)`
  rewrites every key in one call.
- **Named fn/builtin references as callbacks** — a bare identifier that
  names a registered builtin (or user fn) is now a first-class value, so
  you can write `snake_dict.rekey(snake_to_camel)` without wrapping the
  callback in a lambda. Implemented via a new `VmValue::BuiltinRef` variant
  that dispatches through `call_callable_value` at each method-dispatch
  site. Accepted at all dict/list/set/generator method callbacks
  (`map`, `filter`, `reduce`, `find`, `any`, `all`, `sort_by`, `group_by`,
  `max_by`, `map_values`, `rekey`/`map_keys`, etc.). Unknown identifiers
  still error with accurate source carets, and the runtime suggestion
  pool now includes builtin names so typos (e.g. `snake_too_camel`) get
  "did you mean `snake_to_camel`?" hints.
- **`std/path` structural helpers** — new pure-string path manipulation
  builtins in `stdlib/path.rs`: `path_parts`, `path_parent`, `path_basename`,
  `path_stem`, `path_extension`, `path_with_extension`, `path_with_stem`,
  `path_is_absolute`, `path_is_relative`, `path_normalize`, `path_relative_to`,
  `path_to_posix`, `path_to_native`, `path_segments`. All operate on forward
  slashes, collapse `..`, handle Windows drive letters, and never touch
  the filesystem. Rust-side unit tests cover `..` collapse, dot-file stem
  handling (`.gitignore`), and `relative_to` walk-up.
- **Identity / reference equality** — `is_same(a, b)` returns true when
  two heap-allocated values (List/Dict/Set/Closure) share the same
  underlying `Rc` allocation, and falls back to structural equality for
  primitives. `addr_of(v)` returns a stable identity key
  (`list@0x...`, `dict@0x...`, etc.) for bucketing by identity.
- **`hash_value(v)` builtin** — FNV-1a 64-bit hash over a canonical
  display form so structurally-equal values always produce the same hash.
  Non-cryptographic; use `sha256`/`sha512` for integrity.
- **Additional hash algorithms** — `sha224`, `sha384`, `sha512`,
  `sha512_256` alongside the existing `sha256` and `md5`.
- **REPL persistent variable memory** — `let x = 5` followed by `x + 1`
  now works. Successful lines are replayed as prior history on every new
  input; output is diffed so only the newly-executed fragment prints.
  Top-level `fn`/`struct`/`enum`/`type`/`import`/`pub` declarations are
  tracked separately and spliced outside the synthetic pipeline body.
- **REPL implicit println + result history** — bare expressions (`e`,
  `5 + 2`, `find_match("ts")`) are auto-wrapped so their value is both
  displayed and captured under a numbered binding (`_1`, `_2`, ...) for
  later reference in the session.
- **New conformance coverage** — `nested_call_args`,
  `precedence_nil_coalesce_call_args`, `precedence_nil_coalesce_method_chain`,
  `stdlib_case_conversion`, `stdlib_dict_rekey`, `stdlib_identity_hash`,
  `stdlib_path_helpers` (297 → 303 passing).

### Changed

- **Tool-call contract prompts and visible text** — tool-enabled LLM prompts now
  render recursive JSON Schema / OpenAPI shapes into reusable TypeScript-style
  aliases (`$ref`, `oneOf` / `anyOf`, `allOf`, arrays, enums, nested objects),
  and `llm_call(...).visible_text` now removes parsed tool invocations from the
  assistant-visible text while leaving raw `text` and normalized `tool_calls`
  intact.
- **Parser builtin registry alignment** — the parser-side builtin signature
  table now covers far more of the runtime stdlib surface, corrects a number of
  stale static return-type hints, and is guarded by a bidirectional
  parser-vs-runtime alignment test so future builtin additions cannot silently
  drift out of static analysis.
- **ACP boot / trace observability** — bridge mode now emits `ACP_BOOT` timing
  logs for compile, VM setup, and execute phases, and streams `trace_end`
  duration events live instead of waiting for pipeline completion to flush the
  VM output buffer.
- **Ollama reasoning transport** — Ollama requests now always enable the
  provider reasoning channel for thinking-capable models, fall back to
  `message.thinking` when `message.content` is empty, and surface a hard error
  when the server reports generated tokens but returns neither content nor
  reasoning text.
- **Graceful LLM provider error context** — `Missing API key` errors now
  include the currently-loaded `llm.toml` path (or `<built-in defaults>`)
  and the env var names for switching to the mock provider for offline
  experimentation, instead of just naming the missing env var.
- **Shape-typed dicts type-check as dicts for method dispatch** — calling
  `.filter(...)`, `.map_values(...)`, or the new `.rekey(...)` on a
  shape-annotated dict now returns `dict` instead of `list` in the
  inferred type.
- **Case converters & path helpers wired through the typechecker** — all
  new builtin names appear in `is_builtin` and carry explicit return types
  (`string`/`bool`/`dict`/`list`) so downstream type inference works for
  expressions like `let camel: string = snake_to_camel(x)` without
  annotations.

### Fixed

- **Imported module state and sibling function values** — closures imported from
  modules now share top-level `var` / `let` state across calls and can read
  sibling functions both for direct invocation and when passing a function as a
  first-class value.
- **Closure/runtime call edge cases** — local recursive closures late-bind only
  callable names from the caller, dotted dict property calls can invoke stored
  callable values, and `return` inside a nested iterator no longer leaks the
  iterator frame into the caller.
- **Workflow tool handler preservation** — `workflow_graph(...)` and
  `workflow_commit(...)` now retain original tool-handler closures instead of
  dropping them during workflow normalization.
- **Precedence regression pins** — two conformance tests faithfully
  reproduce the two Burin-reported repros for `??` in nested call args
  and chained `??` + `==` with optional chaining. Both pass on current
  main; the minimal self-contained cases no longer trigger the described
  misbehaviour. Tests retained as regression guards so future drift is
  caught immediately.

### Performance / infra

- **cargo-nextest config** — new `.config/nextest.toml` with a 15 s
  slow-test threshold and 60 s hard termination cap (30 s / 60 s
  respectively under `--profile ci`). LLM transport tests have targeted
  overrides since they do real localhost TCP.
- **`make test-fast` target** — runs `cargo nextest run --workspace`
  when nextest is installed; falls back to `cargo test --workspace`
  otherwise. `CONTRIBUTING.md` documents warm-vs-cold timing expectations,
  cold-rebuild triggers, and the optional nextest install.
- **Bounded localhost test stubs** — retrofitted the two old blocking
  ollama stubs in `llm/api.rs` to share a new `accept_with_deadline`
  helper with a 3 s cap and read/write timeouts, matching the pattern
  from `spawn_openai_error_stub`. Stubs can no longer wedge the suite
  indefinitely.

### Deferred to a follow-up

- **Modularity refactor** — splitting the 21 files over 1000 lines into
  submodules (`orchestration.rs`, `commands/portal.rs`, `typechecker.rs`,
  `stdlib/agents.rs`, `compiler.rs`, etc.) is deliberately left out of
  this patch because a parallel session is editing `llm/agent.rs` and
  `llm/tools.rs` concurrently and the mechanical file moves would fight
  the merges. Scope stays roughly as described in the next-session prompt.
- **DRY builtin registry** — `is_builtin` and `builtin_return_type` in
  the typechecker still carry overlapping lists; a single source of
  truth (ideally derived from the stdlib registration) is tracked as a
  follow-up.

## v0.5.37

### Fixed

- **`scan_directory(...)` now agrees with `mkdir`/`write_file` about where
  Harn-relative paths live** — `resolve_scan_root` consulted only the
  execution-context cwd and the process cwd, while the v0.5.36 fs resolvers
  started preferring the active module source dir. The drift silently
  returned an empty list whenever a script `mkdir`'d into its source tree
  and then scanned it, which broke `conformance/tests/metadata_runtime.harn`
  on CI. Both resolvers now share the same priority: execution-context cwd
  → module source dir → process cwd → registration-time base.
- **ACP fatal prompt-load failures now terminate the Harn process cleanly**
  — compile errors, pipeline read failures, and fatal `execute_chunk`
  errors inside an ACP prompt now route through a single
  `exit_after_fatal_prompt_error` helper that emits a `session/update` with
  the formatted error, a JSON-RPC error response for the pending prompt,
  flushes stdio, and exits with code `2`. Hosts (e.g. Burin) that relied on
  the old "send error update and keep running" shape no longer block
  waiting on a still-alive process after a fatal prompt failure.
- **`cargo clippy --workspace --all-targets` passes on a clean tree again**
  — `items_after_test_module` was tripping inside `llm/agent.rs` because
  `register_llm_call_with_bridge` sat after the `#[cfg(test)] mod tests`
  block. The test module now lives at the bottom of the file where clippy
  expects it, and `make lint` plus the pre-commit hook were tightened to
  run clippy with `--all-targets` so the same drift can't land unnoticed
  again.
- **Streaming LLM transport now classifies HTTP errors the same way as the
  non-streaming path** — `vm_call_llm_api` used to return a plain
  `HTTP {status}: {body}` for streaming failures and only tagged
  `[context_overflow]` / `[rate_limited]` / `[http_error]` on the
  non-streaming fallback path. Both branches now share a single
  `classify_http_error` helper, so agent loops get the same tagged
  diagnostics regardless of which transport the provider used. Regression
  tests pin the classification for both streaming (`local` provider stub)
  and direct classifier calls.

### Changed

- **Quieter LLM runtime logs** — unconditional `[llm-debug]` stderr prints
  on every `llm_call` (present in v0.5.36) are gone. Retry/`text_fallback`
  warnings still print because they carry actionable signal.
- **Faster inner-loop builds** — the workspace `[profile.dev]` now uses
  `debug = "line-tables-only"`, shrinking debuginfo and link time without
  losing line-level backtraces. A strict win for incremental dev/test
  cycles on macOS.

## v0.5.36

### Fixed

- **`render(...)` source-relative resolution works again across direct and
  imported calls** — template reads once again resolve from the active module
  source tree instead of drifting to the repo cwd during ordinary runs, which
  fixes the conformance regressions around `fixtures/greeting.prompt` and
  imported helper modules.
- **Imported functions now keep the same runtime setup as in-file
  declarations** — imported top-level `fn` bodies once again carry
  default-argument setup, runtime type checks, generator detection, and
  source-file attribution consistently, so imported helpers behave like local
  ones and report cleaner stack traces.
- **OpenAI-compatible HTTP failures now fail loudly instead of being
  misparsed as empty model output** — non-streaming LLM calls now check HTTP
  status before JSON parsing, classify context-overflow/rate-limit failures,
  and surface provider errors directly so agent loops can react correctly
  instead of retrying blindly against malformed responses.

### Added

- **`std/text` conversion helpers** — `int_to_string`, `float_to_string`,
  `parse_int_or`, and `parse_float_or` are now available from the embedded
  text stdlib so Harn-authored libraries can share explicit conversion
  helpers without reimplementing them in every package.
- **Provider cache token accounting in `llm_call(...)` results** — LLM
  results now expose `cache_read_tokens` and `cache_write_tokens` when
  providers report prompt-cache usage, making warm-up calls and cache hits
  inspectable from Harn itself.

### Changed

- **Nil-coalescing precedence now matches the shipped parser/formatter
  behavior everywhere** — the parser, formatter, formal spec, and language
  docs now agree that `??` binds tighter than additive/comparison/logical
  operators but looser than `* / %`, so expressions like
  `xs?.count ?? 0 > 0` parse as `(xs?.count ?? 0) > 0`.
- **Agent execution is more resilient under heavier tool/LLM workloads** —
  read-only tool batches can prefetch in parallel, transcript dumps now
  record cache-token and response-time metadata, and the default
  `llm_call(...)` output ceiling is raised to `16384` tokens to avoid
  premature truncation in longer repair turns.

## v0.5.35

### Added

- **Embedded React portal frontend** — `harn portal` now serves a Vite-built
  React UI from assets embedded directly into `harn-cli`, so the shipped CLI
  no longer depends on a separate runtime-built `app.js` / `styles.css` pair.
- **Portal launch workspace and playground flows** — the portal can now launch
  existing `.harn` files, run inline scripts, and turn a task plus
  provider/model selection into a persisted playground run that records launch
  metadata and transcript sidecars under the watched run directory.
- **Keyword/highlight generation and docs-snippet verification** — the lexer
  now exposes a canonical `KEYWORDS` list, `dump-highlight-keywords` can
  regenerate `docs/theme/harn-keywords.js`, and `make all` now fails if docs
  ` ```harn ` snippets stop parsing under `harn check`.
- **Portal demo workflow** — `make portal-demo` / `scripts/portal_demo.sh`
  generate a purpose-built demo dataset so the portal can be exercised against
  successful, replay, and failed verification runs without handcrafting sample
  records.

### Changed

- **Portal observability is substantially deeper** — the run list now supports
  filtering, sorting, and pagination; run detail now exposes persisted policy
  summaries, replay metadata, richer stage debug metadata, and more explicit
  failure summaries for faster post-run inspection.
- **Portal frontend development is first-class in repo tooling** — root npm
  scripts, hook checks, and docs now cover portal build/lint/test/dev flows,
  and `harn portal` defaults are documented against the new `4721` / `4723`
  local workflow.
- **Process-relative execution now respects the actual execution cwd** —
  runtime path resolution and the Harn test runner now preserve canonical file
  paths, execute tests from each file’s parent directory when needed, and
  restore the shell cwd afterward so relative file/process behavior matches the
  launched workspace more reliably.
- **Docs/spec/setup surfaces were tightened around the current product shape**
  — README and language/spec docs now consistently describe Harn as an AI-agent
  orchestration runtime, portal/launch behavior is documented coherently, and
  spec examples were updated or marked to avoid snippet-audit drift.

## v0.5.34

### Fixed

- **UTF-8 safe string slicing in transcript compaction** — `microcompact_tool_output()`
  and `truncate_compaction_summary()` now use `floor_char_boundary` /
  `ceil_char_boundary` instead of raw byte offsets, preventing panics on
  multi-byte characters (emoji, CJK, accented text) at slice boundaries.
- **Portal server exits gracefully on bind errors** — invalid addresses and
  listener failures now print a diagnostic to stderr and exit cleanly instead
  of panicking.
- **LSP signature help clamps activeParameter** — the active parameter index
  is now clamped to the parameter list length, preventing out-of-bounds values
  when the cursor has more commas than the function has parameters.
- **Tree-sitter duplicate conflict removed** — `parallel_expression` appeared
  twice in the grammar conflicts array; the duplicate is removed.
- **Text-mode tool-call parser no longer misinterprets `<<` inside quoted
  strings as heredoc openers** — `find_call_block_end` now tracks double-quoted
  string boundaries, so model responses containing heredoc-like syntax in string
  arguments (e.g. `body="<<'EOF'\n...\nEOF"`) parse all call blocks correctly.
- **Sentinel exits now preserve tool-call parse diagnostics** — when an agent
  response hits the phase-loop sentinel, suppressed tool-call parse errors are
  logged to stderr instead of disappearing silently.

### Changed

- **Type checker now visits all compound AST nodes** — the catch-all `_ => {}`
  in `check_node()` has been replaced with explicit arms for 25+ node types
  including `Ternary`, `ThrowStmt`, `GuardStmt`, `SpawnExpr`, `Parallel`,
  `ParallelMap`, `ParallelSettle`, `SelectExpr`, `DeadlineBlock`, `MutexBlock`,
  `Retry`, `Closure`, `ListLiteral`, `DictLiteral`, `RangeExpr`, `Block`,
  `YieldExpr`, `Spread`, `AskExpr`, `Pipeline`, and `OverrideDecl`. This
  ensures the full AST tree is traversed for type analysis and diagnostics.
- **Struct construction field validation** — the type checker now warns on
  unknown fields and missing fields when constructing struct instances.
- **Enum construction variant validation** — the type checker now warns when
  constructing a variant that does not exist in the enum declaration.

### Added

- **Built-in local OpenAI-compatible provider** — Harn now ships a `local`
  provider with `LOCAL_LLM_BASE_URL` / `LOCAL_LLM_MODEL` support, no auth by
  default, and matching docs/spec coverage for self-hosted local model setups.
- **Unicode conformance test** — new `unicode_strings` test exercises string
  operations (len, contains, split, replace, interpolation) with emoji, CJK,
  and accented characters.
- **Editor integration docs** — new `docs/src/editor-integration.md` covering
  LSP capabilities, DAP debugging, tree-sitter grammar, and VS Code setup.
- **Testing guide** — new `docs/src/testing.md` covering the conformance runner,
  `std/testing` host mock helpers, LLM mocking, and assertion builtins.
- **AST spec regenerated** — `spec/AST.md` now reflects the Rust `Node` enum
  with all current variants, replacing the stale Swift-era documentation.

## v0.5.33

### Changed

- **Host capability invocation is now unified on `host_call("capability.operation", ...)`** —
  the live `host_invoke(...)` runtime/ACP path was removed, parser/checker/LSP
  validation now targets the dotted capability contract directly, and shared
  host wrappers/documentation were updated to match.
- **Shared Harn layers are cleaner and less IDE-shaped** — generic runtime and
  session/process helpers now live in `std/runtime`, while host-owned
  filesystem, edit, and IDE/coding-specific wrappers are expected to live in
  product-local `.harn` libraries such as Burin's pipeline libs instead of in
  Harn's shared stdlib.
- **Agent transcript compaction now preserves durable summaries** — prompt
  compaction still shrinks the visible context passed back to the model, but
  the recorded transcript keeps a compaction summary so orchestration and
  conformance surfaces can still explain what happened after the fact.

## v0.5.32

### Changed

- **Text-mode tool calling is more resilient to multiline edits and heredocs**
  — the parser now supports heredoc-style multiline arguments, reports malformed
  ```call``` blocks back to the model instead of silently dropping them, and
  preserves phase-loop exit sentinels even when a response also contains tool
  calls.
- **`apply_edit` now retries dedented multiline patches across both local and
  ACP-backed workspaces** — multiline patch requests that drift in leading
  indentation are retried with a common-indent-stripped old/new pair, so host
  editors and local runs recover the same way from heredoc indentation mismatch.

## v0.5.31

### Changed

- **Tree-sitter parse verification now rebuilds its local parser library when
  grammar sources changed** — `scripts/verify_tree_sitter_parse.py` no longer
  trusts a stale ignored `tree-sitter-harn/harn.dylib`, so release audit stops
  reporting false grammar drift after grammar or generated parser updates.
- **Text-mode tool-call parsing now tolerates trailing literal `\n` before the
  closing fence** — models that emit `edit(...)\\n` inside a ````call` block no
  longer cause the call to be silently dropped when Harn parses function-call
  syntax.

## v0.5.30

### Added

- **`std/testing` now provides higher-level host-mock helpers for Harn tests**
  — Harn scripts can import `clear_host_mocks()`, `mock_host_result(...)`,
  `mock_host_error(...)`, `mock_host_response(...)`, and call assertion helpers
  such as `assert_host_called(...)` and `assert_host_call_count(...)` instead
  of wiring test expectations directly against the lower-level host mock
  builtins.

### Changed

- **Text-mode tool prompting now renders schemas from native tools too** —
  the tool-contract prompt and positional argument inference can now derive
  parameter signatures from host/native tool schemas when a VM tool registry is
  absent, so text-mode agent loops stay usable in native-tool configurations
  instead of silently dropping tool shape information.
- **Harn-owned tool handlers now fail closed without VM execution context** —
  agent loops no longer fall through to bridge `builtin_call` for tools that
  have Harn handler closures attached; if no child VM context is available, the
  call is rejected explicitly so Harn-owned tool execution cannot be silently
  rerouted through the host.
- **LLM transcript dumps now retain resolved tool schema context** — request
  transcript entries now capture the rendered tool schemas used for prompting,
  which makes text-mode tool-call debugging and replay inspection more faithful
  to the actual agent execution surface.

## v0.5.29

### Changed

- **ACP host capability discovery now reflects the actual editor bridge**
  instead of a hard-coded manifest — `host_capabilities()` and `host_has(...)`
  now normalize the live `host/capabilities` response from the ACP client, so
  host-specific capabilities and operations are visible to Harn programs while
  still preserving the typed host contract shape expected by the runtime.
- **ACP workspace mutations now preserve host result payloads** —
  `host_invoke("workspace", "write_text", ...)` now forwards an `overwrite`
  flag when provided and returns the editor bridge's result, and
  `host_invoke("workspace", "apply_edit", ...)` now returns the host response
  instead of discarding it.
- **Interpolated string diagnostics now point at the embedded expression's real
  source location** — lexer string segments now retain line/column metadata, and
  the VM and WASM evaluators re-lex `${...}` expressions from their original
  source position so parse failures inside interpolation report the correct
  location.

## v0.5.28

### Changed

- **Public declarations and generic interfaces are now supported consistently
  across the language toolchain** — `pub pipeline`, `pub enum`, and `pub struct`
  now parse in the Rust compiler path, generic interfaces and generic interface
  methods are preserved in the AST/formatter/LSP, and the tree-sitter grammar
  plus conformance coverage now match the formal language surface.
- **Parser diagnostics now point at the real failure location** — unexpected EOF
  errors carry source spans instead of collapsing to a dummy `0:0` location, and
  the CLI/LSP now render parser-specific messages and EOF help text instead of a
  single generic “unexpected token” label.
- **Language docs and executable coverage were tightened together** — the
  language basics guide now shows public declarations and generic interfaces,
  and a new conformance case exercises the public declaration surface so future
  parser/editor regressions are caught in release audit.

## v0.5.27

### Changed

- **`harn-cli` now uses a declarative clap command graph instead of hand-rolled
  argv parsing** — top-level commands, `mcp` subcommands, and `add` now share a
  single typed parser with conflict validation, structured help text, and
  parser coverage tests instead of ad hoc `env::args()` scanning.
- **CLI validation is stricter and less error-prone** — conflicting flag
  combinations such as `--deny` plus `--allow` are enforced by the parser, `run`
  now requires exactly one of inline code or a file, and nested command surfaces
  reject malformed input before reaching runtime handlers.
- **Operator-facing help is now generated from command metadata** — `harn help`
  and subcommand help output now describe each command and option directly from
  the source-of-truth CLI definitions, removing the duplicate hand-maintained
  help implementation.

## v0.5.26

### Changed

- **Conformance test targeting now behaves like a real selector** — `harn test
  conformance <file-or-dir>` now resolves a concrete file or subtree under
  `conformance/`, rejects missing or out-of-tree targets, and no longer falls
  back to running the entire suite when the user intended a narrow run.
- **Conformance discovery now recurses through the whole suite tree** — the CLI
  walks nested directories instead of only the root plus one level of
  subdirectories, so deeper conformance fixtures are picked up consistently.
- **CLI help and docs now document targeted conformance runs** — the built-in
  help, README, and CLI reference now show the supported single-file workflow,
  and `harn test` rejects extra positional arguments instead of silently
  ignoring them.

## v0.5.25

### Changed

- **Text-mode tool calling no longer bakes in IDE-specific tool names** — the
  runtime-owned contract and agent nudge examples now stay generic, and
  positional text-call parsing only infers a parameter name from the live tool
  schema when a tool declares exactly one parameter.
- **Scalar argument parsing remains type-stable in text mode** — floating-point,
  boolean, integer, and `null` values in ` ```call ` blocks continue to round-trip
  as structured JSON values instead of silently degrading into strings.
- **Worktree conformance uses collision-resistant temp paths** — the runtime
  worktree fixture now adds a random suffix instead of relying on second-level
  timestamps alone, which removes flaky temp-directory collisions during rapid
  audit and release reruns.

## v0.5.24

### Changed

- **Text-mode tool calling is stricter and more type-stable** — the
  runtime-owned tool contract now explicitly lists the allowed tool names while
  still warning models not to treat argument names like `file_glob` as tools,
  and text-call parsing now preserves floating-point, boolean, integer, and
  `null` scalar argument types instead of degrading some values into strings.
- **Tool-contract coverage now pins the prompt and parser behavior together** —
  `harn-vm` adds regression tests for JSON-array command recovery, scalar JSON
  parsing, and tool-registry prompt rendering so text-call behavior is checked
  under the same stricter quality gates used by release audit.

## v0.5.23

### Added

- **Dict iteration helpers and conditional template sections** — Harn now
  ships `keys(dict)`, `values(dict)`, and `entries(dict)` builtins for
  dictionary introspection, and `render(...)` / dict-backed `format(...)`
  templates can now include `{{if key}}...{{end}}` blocks that render only
  when the bound value is truthy.

### Changed

- **`tool_define(...)` now requires `parameters` instead of legacy `params`**
  — tool registries normalize JSON Schema input definitions under the
  `parameters` key, reject the old `params` spelling with an explicit runtime
  error, and the public docs/examples now match the enforced schema.

## v0.5.22

### Added

- **Typed host mocks for VM and conformance tests** — `host_mock(...)`,
  `host_mock_clear()`, and `host_mock_calls()` now let Harn programs register
  runtime host-operation fixtures, override specific capability/operation
  pairs by matching on partial params, inspect recorded invocations, and
  simulate host-thrown errors without requiring a bridge host.

## v0.5.21

### Changed

- **Workflow tool registries now carry enforceable runtime policy metadata** —
  `tool_define(...)` entries can now include policy descriptors such as
  capabilities, side-effect level, mutation classification, and declared path
  parameters, and workflow validation/execution intersects those descriptors
  with the active ceiling automatically instead of relying only on manually
  duplicated node policy blocks.
- **Verify stages can execute commands directly and assert exit status** —
  workflow `verify` nodes may now run a shell command inside the current
  execution context, record stdout/stderr on the stage result, and evaluate
  both `assert_text` and `expect_status` checks without routing verification
  through an LLM/tool loop.
- **Local tool execution now respects workflow execution context more
  consistently** — ACP chunk execution seeds the runtime execution context, and
  VM-local `read_file` / `list_directory` resolution now honors the current
  working directory when workflows or delegated runs provide one.

## v0.5.20

### Added

- **LLM API transcript capture** — full LLM call request/response payloads are now
  optionally written to `llm_transcript.jsonl` in a directory set by
  `HARN_LLM_TRANSCRIPT_DIR`, including call metadata, token usage, and
  request/response content.

### Changed

- **Formatter precedence rendering for postfix chains and unary operands** — `harn-fmt`
  now preserves parentheses around complex expressions before method calls, property
  access, optional chaining, indexing/slicing, and try postfixes, while also
  preserving parentheses for appropriate operands of unary operators to keep output
  both valid and stable.

## v0.5.19

### Added

- **LLM call retry logic** — transient errors (HTTP 429, 500, 502, 503, 529,
  connection timeouts) are retried with exponential backoff. Configurable via
  `llm_retries` (default 2) and `llm_backoff_ms` (default 2000). Retry-After
  headers are parsed and respected. Non-retryable errors (400, 401, 403) abort
  immediately.
- **Graceful shutdown** — CLI installs SIGTERM/SIGINT handler that gives the VM
  2 seconds to flush run records before exit(124).
- **Atomic run record persistence** — `save_run_record` writes to a `.tmp` file
  then renames, preventing corruption from mid-write kills.
- **Enhanced microcompaction diagnostics** — file:line pattern recognition,
  expanded keyword set (cannot find, not found, unresolved, missing, mismatch,
  unused), increased diagnostic line limit from 24 to 32.
- **Runtime-owned tool-calling contract** — system prompt injection declares the
  active mode (`text` or `native`) and overrides any stale prompt text.
- **Text fallback trace logging** — emits a warning when native mode falls back
  to text-call parsing.
- **Ollama runtime overrides** — `BURIN_OLLAMA_NUM_CTX`, `OLLAMA_NUM_CTX`,
  `BURIN_OLLAMA_KEEP_ALIVE`, and `OLLAMA_KEEP_ALIVE` env vars are injected into
  Ollama API requests.
- **Workflow stage metadata** — stage results now include prompt, system_prompt,
  rendered_context, selected artifacts, and tool_calling_mode for inspection.

### Changed

- **Stage outcome classification refactored** — extracted into
  `classify_stage_outcome()` with correct handling of `stuck` and `done` agent
  statuses.
- **Agent loop nudge messages** — text-mode nudges now include concrete
  `​```call` examples instead of generic "use tools" instructions.

## v0.5.18

### Changed

- **Agent workflows can now choose their own completion sentinel** — the VM
  accepts `done_sentinel` in agent-loop and workflow-node options, threads it
  through orchestration, and stops persistent agents on the configured marker
  instead of the hard-coded `##DONE##`.
- **Tool execution is more resilient across native and text-call providers** —
  workflow agent stages now prefer provider-native tool calls when available
  while still accepting fenced text-call fallbacks, rejected tool calls feed a
  direct follow-up instruction back into the loop, and the text tool prompt is
  stricter about avoiding redundant discovery calls when the prompt already
  contains the needed file and path context.

## v0.5.17

### Changed

- **Wrapped selective imports now use standard trailing commas** — long
  `import { ... } from "..."` declarations now format one imported name per
  line with a trailing comma before `}`, matching the formatter's other
  multiline comma-separated forms and the parser's accepted syntax.
- **Tree-sitter and release tooling now match the formatter's import layout** —
  the editor grammar accepts trailing-comma selective imports, the corpus
  covers the wrapped form, and the local `harn-release` skill now has valid
  YAML frontmatter so release automation can load it cleanly.

## v0.5.16

### Changed

- **ACP bridge `llm_call` no longer runs provider I/O on the LocalSet** —
  bridge-aware LLM calls now split VM-local options from a Send-safe transport
  payload and execute the actual HTTP/TLS request on Tokio's multithreaded
  scheduler before returning to the LocalSet for transcript assembly and host
  notifications. This fixes the nested ACP sub-VM hang against cloud HTTPS
  providers while preserving the existing bridge event model.
- **The LLM transport boundary is now explicit and testable** — added a
  dedicated `LlmRequestPayload` transport struct plus a LocalSet regression
  test that drives an Ollama-style streaming response through the off-thread
  path, so this scheduling bug is pinned down by executable coverage instead of
  a local repro only.
- **`harn-vm` explicitly enables Tokio's multithread runtime** — the VM crate
  now declares `rt-multi-thread` in its Tokio feature set so the same runtime
  topology used by ACP is available in verification and release builds.

## v0.5.15

### Changed

- **Changelog-backed release-note rendering now works on GitHub runners** —
  `scripts/render_release_notes.py` no longer shells out to `zsh` to discover
  `GITHUB_REPOSITORY`, so the `Create Release` workflow can render notes on the
  stock Ubuntu runner and complete the release automatically.
- **Release automation is fully wired end to end again** — the remaining
  `v0.5.14` failure mode was isolated to the release-notes renderer rather than
  the build matrix, and this patch removes that final workflow portability bug.
- **Conformance output comparison is stable for timer lines** — the Harn test
  runner now normalizes `[timer] ...: Nms` output before comparing against
  `.expected` files, eliminating clock-jitter flakes in `conformance`.
- **Local LLM API debug tracing was folded into the patch sweep** — the current
  local debug logging in `crates/harn-vm/src/llm/api.rs` is now included in the
  audited release candidate instead of being left behind as a machine-local
  change.

## v0.5.14

### Changed

- **Release binaries no longer depend on Linux DBus development headers** —
  `harn-cli` now uses the native Linux `keyring` backend without the
  `sync-secret-service` DBus feature, which fixes the GitHub release workflow's
  Linux packaging failure while preserving native macOS, Windows, and Linux
  credential storage support.
- **Release automation is closer to fully hands-off again** — the remaining
  post-`0.5.13` failure mode in the binary release workflow was traced to
  `libdbus-sys` packaging requirements rather than OpenSSL/TLS, and this patch
  removes that blocker from the default CLI build.

## v0.5.13

### Changed

- **Workspace HTTP clients now prefer Rustls over native TLS** — `harn-vm`
  and `harn-cli` both disable `reqwest`'s default TLS stack and use
  `rustls-tls` explicitly, folding in the local Burin-agent fix and reducing
  OpenSSL-related friction in cross-platform builds and release automation.
- **The next patch release explicitly includes locally discovered release
  fixes** — release hygiene now treats local integration fixes as first-class
  patch content instead of leaving them stranded as untracked or unreviewed
  machine-local changes.

## v0.5.12

### Added

- **Mutation-session audit metadata across the runtime** — workflow runs,
  delegated workers, and bridge tool hooks now carry structured mutation
  session context so hosts can group writes, approvals, and artifacts under a
  coherent trust boundary.
- **Executable release gate and publish ritual** — added
  `scripts/release_gate.sh` plus reusable Codex wrappers so audit, version
  bump, publish, tagging, and release prep follow one repo-native workflow.
- **Language-spec verification loop** — added `scripts/verify_language_spec.py`
  and `scripts/sync_language_spec.sh`, promoted `spec/HARN_SPEC.md` into the
  release gate, and hosted the spec from the mdBook site via
  `docs/src/language-spec.md`.
- **Strict tree-sitter conformance sweep** — added
  `scripts/verify_tree_sitter_parse.py` to run the positive `.harn` corpus
  through the executable tree-sitter grammar as part of the final verification
  loop.
- **Layout-aware tree-sitter scanner** — added
  `tree-sitter-harn/src/scanner.c` so multiline layout-sensitive constructs can
  be parsed consistently in the editor grammar.

### Changed

- **Bridge and worker lifecycle payloads are richer and more host-friendly** —
  worker updates now include structured lifecycle metadata, child-run linkage,
  timing, snapshot paths, and mutation-session context instead of leaving hosts
  to infer those details from logs.
- **Tree-sitter grammar now handles multiline and postfix forms more
  consistently** — fixed multiline calls, multiline operators, interpolated
  strings, property/method postfix chains, and related recovery drift.
- **Release notes can now be sourced from `CHANGELOG.md`** — the repo can
  render version-specific GitHub release notes locally so the release page does
  not depend on GitHub’s auto-generated summary.
- **Security and host-boundary docs are more explicit** — documentation now
  covers remote MCP OAuth implications, proposal-first write guidance,
  worktree-first autonomous execution, and the division of responsibility
  between Harn and host integrations such as Burin.

## v0.5.11

### Added

- **Standalone remote MCP OAuth in the CLI** — added `harn mcp login`,
  `harn mcp logout`, `harn mcp status`, and `harn mcp redirect-uri` so Harn
  can authorize directly against remote MCP servers instead of requiring hosts
  to inject bearer tokens manually.
- **Manifest-level remote MCP OAuth config** — `[[mcp]]` entries can now set
  `transport = "http"` plus `url`, `client_id`, `client_secret`, and
  `scopes`, allowing pre-registered OAuth clients and advanced deployments to
  supply their own credentials while still benefiting from metadata discovery
  and token refresh.
- **ACP host-provided MCP loading** — ACP sessions now automatically consume
  host-provided MCP server config and expose connected clients through the
  global `mcp` dict, aligning embedded editor flows with standalone manifest
  execution.

### Changed

- **Remote MCP clients now auto-load stored OAuth tokens** — `harn run` will
  reuse and refresh previously stored tokens for HTTP MCP servers declared in
  `harn.toml`, so remote servers behave like first-class runtime dependencies
  instead of ad hoc per-run configuration.
- **HTTP MCP transport is more resilient** — the VM now recovers from expired
  MCP HTTP sessions by re-running the initialize handshake, and it auto-detects
  SSE-framed JSON-RPC responses in addition to plain JSON bodies.
- **OAuth metadata discovery is path-aware** — Harn now checks protected
  resource metadata and authorization server metadata using the latest MCP
  discovery patterns instead of assuming only origin-root well-known URLs.
- **Agent tool output defaults are simpler** — the runtime now defaults tool
  formatting to `text` instead of `native`, reducing structured-wrapper noise
  in common agent transcripts.
- **OpenAI-style provider normalization is stricter for Ollama-compatible
  responses** — OpenAI-style message blocks are normalized into text for
  Ollama-compatible transports, and stream handling is enabled consistently for
  those providers.

## v0.5.10

### Changed

- **`match` now parses correctly as an expression** — fixed the parser entry
  point so bindings like `let x = match value { ... }` compile correctly,
  including match arms that declare local `let` bindings before yielding a
  final expression.

## v0.5.9

### Added

- **Reusable typed host wrappers in `std/project`** — added
  `workspace_roots(...)`, `workspace_read_text(...)`,
  `workspace_write_text(...)`, `workspace_apply_edit(...)`,
  `workspace_delete(...)`, `workspace_list(...)`,
  `workspace_exists(...)`, `workspace_file_exists(...)`,
  `process_exec(...)`, and `interaction_ask(...)` so hosts can share one
  portable adapter surface instead of redefining generic workspace/process
  helpers in host-local modules.
- **Richer run-record metadata handoff** — `record_run_metadata(...)` now
  forwards usage totals, transcript counts, summary text, and persisted-path
  metadata alongside workflow/status fields so host bridges can adopt session
  cost and continuity UIs without re-deriving that data from raw traces.
- **Linux ARM64 release assets** — the GitHub release workflow now builds and
  packages `aarch64-unknown-linux-gnu` tarballs alongside the existing macOS
  and Linux x64 artifacts.

### Changed

- **`harn check` and `harn lint` now operate on multiple files/directories** —
  the CLI now matches `harn fmt` target semantics, recursively collecting
  `.harn` files from directories and aggregating failures across all targets.
- **Host-capability preflight aligns with typed host integrations** — the
  checker now recognizes the common workspace/runtime/project/editor/git/
  diagnostics/learning capability families and fully honors external
  host-capability manifests for multi-file validation.
- **ACP and local host manifests are more consistent** — workspace capability
  aliases such as `file_exists` and `project_root` now round-trip through the
  local VM host adapter, ACP manifest reporting, and `host_has(...)` checks.

## v0.5.8

### Added

- **Versioned multimodal transcript assets** — transcript values now preserve
  durable asset descriptors alongside block-structured messages and canonical
  events, so image/file/document attachments survive export, compaction, fork,
  and replay without inlining large payloads.
- **Workflow session helpers** — added `workflow_session_new(...)`,
  `workflow_session_restore(...)`, `workflow_session_fork(...)`,
  `workflow_session_archive(...)`, `workflow_session_resume(...)`,
  `workflow_session_compact(...)`, `workflow_session_reset(...)`, and
  `workflow_session_persist(...)` in `std/agents` for host-neutral chat/session
  lifecycle management on top of transcripts and run records.
- **Ad hoc run-record persistence helpers** — added
  `workflow_result_text(...)`, `workflow_result_run(...)`, and
  `workflow_result_persist(...)` so hosts can persist non-`workflow_execute`
  agent results as first-class Harn run records instead of inventing parallel
  session formats.
- **Workflow/session usage summaries** — run records and `workflow_session(...)`
  now preserve cumulative token/duration/call-count usage so host UIs can show
  one canonical session cost summary instead of recomputing it from ad hoc
  traces.

### Changed

- **Transcript messages and events now preserve structured blocks** — visible
  text, tool calls/results, private reasoning, and multimodal references
  round-trip through transcript import/export without flattening to plain text.
- **Transcript lifecycle semantics are explicit** — fork/archive/resume/reset
  operations now append canonical lifecycle events and retain asset state
  consistently across worker snapshots and run records.
- **Host-side session restore can now key off transcript visibility tiers** —
  transcript events clearly distinguish `public`, `internal`, and `private`
  execution history for clean IDE/UI presentation without duplicating
  orchestration policy in the host.
- **Trace/session usage plumbing is unified** — LLM trace summaries now feed
  run-record stage usage and workflow session state consistently, making
  replay, inspector views, and persisted chat summaries agree on the same
  totals.

## v0.5.6

### Added

- **Structured schema runtime helpers** — added `schema_check(...)`,
  `schema_parse(...)`, `schema_to_json_schema(...)`, `schema_extend(...)`,
  `schema_partial(...)`, `schema_pick(...)`, and `schema_omit(...)` for
  runtime validation, defaulting, JSON Schema export, and schema composition.
- **Design-by-contract with `require`** — added a `require condition, "message"`
  statement for lightweight runtime precondition checks in pipelines and
  functions.
- **Project metadata/runtime inventory helpers** — added `metadata_resolve(...)`,
  `metadata_entries(...)`, `metadata_status(...)`, and an options-aware
  `scan_directory(...)`, plus a new `std/project` module for freshness-aware
  project state assembly inside Harn code.
- **HarnDoc enforcement for public APIs** — `harn lint` and `harn check` now
  report `missing-harndoc` when `pub fn` APIs lack a contiguous `///` doc block.

### Changed

- **`scan_directory(...)` now follows execution cwd semantics** — relative scan
  paths now resolve the same way as runtime file/process operations instead of
  the VM registration base, fixing incorrect project scans in embedded hosts.
- **LSP/hover doc extraction prefers HarnDoc** — contiguous `///` comments are
  now the canonical documentation source, with plain `//` comments retained
  only as a fallback for hover text.

## v0.5.5

### Added

- **Pluggable transcript auto-compaction** — `agent_loop` now supports
  `compact_strategy: "llm" | "truncate" | "custom"` with LLM-powered
  compaction as the default strategy, plus `compact_callback` for custom
  Harn closures. Added `transcript_auto_compact(messages, options?)` for
  invoking the same pipeline outside agent loops.
- **Daemon wake protocol and adaptive backoff** — daemon agents now idle with
  exponential backoff (`100ms`, `500ms`, `1s`, `2s`) and can be resumed via
  bridge `agent/resume` notifications or queued user messages.
- **Bridge protocol documentation** — documented `tool/pre_use`,
  `tool/post_use`, `agent/idle`, and `agent/resume` host/runtime messages.
- **Extensible `harn check` host capability validation** — preflight now
  accepts host-specific capability schemas from `[check].host_capabilities`,
  `[check].host_capabilities_path`, or `--host-capabilities <file>`.
- **Bundle-aware preflight path validation** — `harn check` accepts
  `[check].bundle_root` or `--bundle-root <dir>` so `render(...)` and
  `host_invoke("template", "render", ...)` can validate against bundled
  layouts as well as source layouts.
- **String case helpers** — string methods now include `.lower()`,
  `.upper()`, `.to_lower()`, and `.to_upper()`.
- **Conformance coverage for agent/runtime integration points** — added
  end-to-end cases covering tool-hook rejection/truncation, policy-driven tool
  argument rejection, adaptive artifact deduplication, and transcript
  auto-compaction configuration.

### Changed

- **Auto-compaction defaults are semantic instead of truncation-only** —
  agent-loop compaction now preserves more task context by defaulting to an
  LLM summary rather than fixed-size message truncation.
- **`harn check` preflight is host-extensible instead of host-hostile** —
  host adapter pipelines can declare their own capability surfaces rather than
  failing static validation on non-core host operations.

## v0.5.4

### Added

- **Tool lifecycle hooks** — `register_tool_hook({pattern, deny?, max_output?})`
  and `clear_tool_hooks()` enable pre/post-execution interception of tool calls
  in agent loops. Pre-hooks can deny with a reason; post-hooks can truncate
  oversized tool output. Hooks fire through glob-matched patterns (e.g.
  `"exec*"`, `"*"`) and are wired into the agent loop's tool dispatch.
- **Automatic multi-strategy transcript compaction** — `agent_loop` now accepts
  `auto_compact: true` with configurable `compact_threshold`,
  `compact_keep_last`, and `tool_output_max_chars`. Microcompaction snips
  oversized individual tool outputs; auto-compaction triggers when estimated
  tokens exceed the threshold and summarizes older messages in-place.
- **Daemon agent mode** — `agent_loop` accepts `daemon: true` for agents that
  stay alive waiting for host-injected messages instead of terminating on
  text-only responses. Emits `agent/idle` bridge notifications when idle.
- **Per-agent capability policy** — `agent_loop` accepts `policy: {...}` to
  scope tool permissions per-agent. Policies are pushed/popped on the execution
  stack automatically. Supports `tool_arg_constraints` for argument-level
  pattern matching (e.g. allow `exec` only for `cargo *` commands).
- **Adaptive context assembly** — `select_artifacts_adaptive(artifacts, policy)`
  deduplicates artifacts by content, microcompacts oversized ones to fit within
  budget, then delegates to standard token-aware selection.
- **New builtins**: `estimate_tokens(messages)`, `microcompact(text, max_chars)`.

## v0.5.3

- Fixed `render(...)` resolution in imported modules: templates are now
  resolved relative to the importing module's source directory, not the
  entry pipeline. This applies consistently across source runs, bundled
  runs, ACP execution, and `host_invoke("template", "render")`.
- Added import symbol collision detection at both runtime and preflight.
  When two wildcard imports export the same function name, Harn now emits
  an actionable diagnostic with guidance to use selective imports.
- Strengthened `harn check` preflight with host capability contract
  validation: `host_invoke(...)` calls referencing unknown capabilities
  or operations are now flagged before runtime.
- Added conformance test for imported module render resolution and unit
  tests for import collision detection and host capability validation.

## v0.5.2

- Added worker execution profiles with cwd/env overlays and managed worktree
  preparation so delegated runs can execute in isolated, reproducible roots.
- Added delegated run-tree lineage to persisted run records plus
  `load_run_tree(...)` for recursive inspection of child runs.
- Strengthened `harn check` to validate literal delegated execution roots and
  `exec_at(...)` / `shell_at(...)` directories before runtime.
- Added review/apply-oriented artifact helpers:
  `artifact_patch_proposal(...)`, `artifact_verification_bundle(...)`, and
  `artifact_apply_intent(...)`.
- Made the formatter wrap oversized comma-separated inline forms consistently
  across calls, list literals, dict literals, enum payloads, and struct-style
  construction.

## v0.5.1

- Added persisted worker snapshots, resumable delegated workers, and `std/worktree`
  helpers for isolated git worktree execution.
- Added `exec_at(...)` and `shell_at(...)` plus worker carry policy controls for
  transcript, artifact, and workflow continuation behavior.
- Strengthened `harn check` with import/resource preflight validation and fixed
  `render(...)` / template path resolution to follow pipeline source roots.
- Split the delegated worker runtime into smaller Rust modules and added new
  runtime and CLI coverage around snapshots, worktrees, and preflight checks.

## v0.5.0

### Added

- **Delegated worker runtime** — `spawn_agent(...)`, `send_input(...)`,
  `wait_agent(...)`, `close_agent(...)`, and `list_agents()` add a first-class
  worker/task lifecycle to Harn's orchestration surface.
- **Delegated workflow stages** — `subagent` workflow nodes now execute through
  the same worker runtime and attach worker lineage to produced artifacts and
  stage metadata.
- **Host-visible worker events** — bridge/ACP hosts now receive structured
  worker lifecycle updates with lineage, artifact counts, transcript presence,
  and child run identifiers for delegated work.
- **Worker lifecycle conformance coverage** — new conformance cases cover
  worker spawn/wait/continue/close flows and delegated workflow node execution.

### Changed

- **Workflow runs record delegation lineage** — delegated stages now persist
  worker summaries into stage metadata so replay/eval and future host UIs can
  inspect child execution boundaries.
- **Delegated stage artifacts carry provenance** — artifacts emitted by
  delegated stages are tagged with worker metadata and a delegated marker.
- **Release version raised to 0.5** — crate interdependencies now target the
  0.5 series to match the expanded orchestration runtime surface.

## v0.4.32

### Added

- **Eval suite manifests** — grouped replay/eval suites are now a typed
  runtime surface via `eval_suite_manifest(...)`, `eval_suite_run(...)`, and
  `harn eval <manifest.json>`, with optional baseline run comparisons per case.
- **Host artifact helper builtins** — new artifact constructors cover
  workspace files, workspace snapshots, editor selections, command results,
  verification/test outputs, unified diffs, git diffs, review items, and
  accept/reject decisions.
- **Regression coverage for artifact/review flows** — conformance and VM tests
  now cover eval manifests, diff/review artifacts, baseline comparison
  reporting, IEEE float division semantics, and repeated catch bindings.

### Changed

- **CLI replay/eval inspection is more useful** — `harn eval` now accepts a
  manifest file in addition to single run records or run directories, and
  suite case output includes baseline diff status when comparisons are present.
- **Artifact taxonomy is more explicit** — `workspace_snapshot`, `git_diff`,
  `patch_set`, `diff_review`, and `review_decision` are normalized built-in
  artifact kinds with default priority and provenance-friendly helper APIs.
- **Typechecker/LSP/docs stay aligned with runtime growth** — the new eval and
  artifact helper builtins are recognized statically, surfaced in hover and
  signatures, and documented in the runtime and CLI references.

### Fixed

- **Repeated catch bindings in the same block** — sibling `try/catch`
  expressions can now reuse the same catch variable name without tripping
  same-scope immutable redeclaration errors.
- **Float divide-by-zero semantics** — floating-point division preserves IEEE
  `NaN`/`Infinity` behavior while integer division by zero still fails.
- **Release hygiene around run artifacts** — `.harn-runs/` is ignored by git so
  persisted run records stop polluting release working trees.

## v0.4.31

### Added

- **Workflow replay fixtures and regression assertions** — run records now
  produce explicit replay fixtures with stage assertions, workflow diff data,
  and eval diagnostics that can be consumed from both the CLI and host code.
- **Policy-aware transcript lifecycle controls** — transcript reset, archive,
  abandon, resume, visible/full rendering, and canonical event separation are
  now covered by runtime builtins, conformance tests, and host-facing docs.
- **Tree-sitter workflow/runtime corpus coverage** — corpus tests now cover
  workflow/runtime builtin-heavy programs so parser and highlighting regressions
  show up in CI instead of after release.

### Changed

- **Workflow runtime semantics are more explicit** — condition, fork/join,
  map/reduce, escalation, checkpoint, transition, and replay state all use
  typed runtime records rather than status-string inference.
- **Artifact selection is now a real context budgeter** — built-in artifact
  kinds are normalized and ranked by priority, freshness, recency, pins, kind
  preference, stage filters, and reserved token budget.
- **Policy reporting accepts explicit ceilings** — `workflow_inspect(...)` and
  `workflow_policy_report(...)` now let hosts inspect a graph against a real
  upper bound instead of only the permissive builtin ceiling.

### Fixed

- **Bridge and MCP policy escape hatches closed** — unknown bridged builtins
  and MCP client operations are now rejected under active execution ceilings
  instead of bypassing workflow policy composition.
- **Typechecker/runtime builtin drift reduced** — new workflow, replay,
  artifact, and transcript builtins are recognized by static type inference and
  LSP signatures.
- **Conformance and tree-sitter release coverage** — workflow policy guardrail
  assertions now validate against an explicit restrictive ceiling, and the
  tree-sitter corpus matches the current grammar.

## v0.4.30

### Added

- **Typed workflow runtime** — `workflow_graph()`, `workflow_validate()`,
  `workflow_execute()`, and workflow edit builtins now provide a typed
  orchestration graph layer above raw `task_run`-style helpers.
- **Typed artifacts/resources** — first-class artifact records now support
  provenance, lineage, relevance, token estimates, and policy-driven context
  selection via `artifact()`, `artifact_derive()`, `artifact_select()`, and
  `artifact_context()`.
- **Durable run records and CLI inspection** — workflow executions now persist
  structured run records with stage data, transcripts, artifacts, and policy
  metadata. New CLI commands: `harn runs inspect`, `harn replay`, and
  `harn eval`.
- **Canonical transcript event model** — transcripts now carry normalized
  `events`, with helpers for visible rendering, full rendering, compaction,
  summarization, forking, export/import, and lifecycle management.
- **Provider-normalized response schema** — `llm_call()` and `agent_loop()`
  now expose canonical `visible_text`, `private_reasoning`, `provider`,
  `blocks`, and normalized tool-call metadata across providers and mocks.
- **Queued human-message delivery modes for ACP/bridge hosts** — agent loops
  now support `interrupt_immediate`, `finish_step`, and
  `wait_for_completion` delivery semantics inside the runtime.

### Changed

- **`workflow_run()` removed** — it had become a dead narrow wrapper over
  `workflow_execute()`. `task_run()` remains the compatibility helper, and
  `workflow_execute()` is the direct runtime entrypoint.
- **Workflow execution is more inspectable** — stage records now include
  policy metadata, verification outcome fields, transcript policy effects,
  and persisted run-path handling.
- **Docs and help surfaces updated** — README, docs book, CLI reference,
  and contributor guidance now reflect the workflow/artifact/run-record
  runtime and current ACP usage.

### Fixed

- **Capability-ceiling enforcement** — workflow validation now explicitly
  rejects attempted privilege expansion relative to the runtime ceiling.
- **Queued message tests** — bridge-side queued-message behavior is covered
  by runtime tests without relying on `tokio::test`.

## v0.4.29

### Added

- **Typed host capabilities** — `host_capabilities()`, `host_has()`, and `host_invoke()`
  provide a typed host abstraction for workspace, process, template, and
  interaction operations in both native and ACP runtimes.
- **Transcript-aware LLM orchestration** — `llm_call()` and `agent_loop()`
  now return `transcript`, and new transcript builtins support export/import,
  fork, compaction, and LLM-assisted summarization for long-running agent work.
- **`llm_completion()` builtin** — Harn now owns text completion / FIM as an
  LLM primitive, using provider-native completion endpoints where available and
  a Harn fallback path otherwise.
- **Model-tier routing** — `llm_pick_model()` resolves aliases or tiers such as
  `small`, `mid`, and `frontier` into concrete `{id, provider, tier}` model
  selections, with built-in default aliases.
- **Structured context and workflow modules** — new embedded `std/context` and
  `std/agents` modules provide prompt assembly, context sections, transcript
  continuation, `task_run()`, verification, repair, and workflow compaction.

### Changed

- **Host process execution results are structured** — `host_invoke("process", "exec", ...)`
  now returns `{stdout, stderr, combined, status, success}` instead of a flat string.
- **Workspace listing is richer** — `host_invoke("workspace", "list", ...)`
  now returns entry dicts with `name`, `path`, and `is_dir`.

### Fixed

- **ACP typed-host parity** — ACP now exposes the same typed host capability
  surface and normalized process execution results as the local runtime.

## v0.4.28

### Breaking changes

- **`llm_call` always returns a dict** — previously returned a plain string
  for simple calls. Now always returns `{text, model, input_tokens,
  output_tokens}`. Use `.text` to get the string content.
- **`think` option renamed to `thinking`** — expanded semantics: `true` for
  provider defaults, or `{budget_tokens: N}` for explicit budget. Works
  across Anthropic (thinking blocks), OpenAI (reasoning), and Ollama.
- **`--bridge` flag removed** — bridge protocol replaced by ACP. Use
  `harn acp` instead of `harn run --bridge`.

### Added

- **Consolidated `LlmCallOptions` struct** — replaces 12 positional parameters
  internally. All LLM builtins now share a single option extraction path.
- **New LLM options** — `top_p`, `top_k`, `stop` (stop sequences), `seed`,
  `frequency_penalty`, `presence_penalty`, `tool_choice`, `cache` (Anthropic
  prompt caching), `timeout`, and provider-specific override sub-dicts
  (`anthropic: {}`, `openai: {}`, `ollama: {}`).
- **Extended thinking support** — `thinking: true` or `thinking: {budget_tokens: N}`
  works for Anthropic, OpenAI, and Ollama. Response includes `thinking` and
  `stop_reason` fields.
- **Anthropic structured output** — `response_format: "json"` with `schema`
  now works for Anthropic via synthetic tool-use constraint pattern.
- **Provider option validation** — runtime warnings when passing options
  not supported by the target provider (e.g., `seed` on Anthropic).
- **ACP builtins expanded** — `apply_edit`, `delete_file`, `file_exists`,
  `host_call`, `render`, `ask_user`, `run_command` added to ACP server.

### Removed

- **`bridge_builtins.rs`** — entire bridge protocol layer removed. ACP is
  now the only host integration protocol.
- **`run_file_bridge()`** — removed from CLI.

### Fixed

- **Default unification** — `max_tokens` = 4096, `max_nudges` = 3,
  `max_iterations` = 50 everywhere (previously varied between bridge and
  non-bridge modes).
- **`llm_stream` alignment** — now supports `messages`, `temperature`, and
  other options (previously only accepted flat prompt string).

## v0.4.27

### Added

- **Tree-sitter grammar overhaul** — syntax highlighting now supports all
  current features: `enum`, `struct`, `impl`, `interface`, `in`/`not in`,
  `%`, `yield`, `deadline`, `guard`, `break`/`continue`, `finally`,
  `mutex`, `select`, duration literals, compound assignment, spread,
  try-expression, `?` operator, generic params, where clauses, destructuring.
- **Typechecker: full interface method signature checking** — `where T: Interface`
  constraints now verify param types and return types, not just method names
  and param counts.
- **VM error source locations** — runtime errors now consistently include
  `(line N)` for all error types (Runtime, TypeError, DivisionByZero,
  UndefinedVariable, etc.).
- **LSP hover for local functions** — shows signature, doc comments, and
  impl type context.

### Fixed

- **`produces_value` missing entries** — `EnumDecl`, `InterfaceDecl`, and
  `TypeDecl` now correctly marked as non-value-producing, fixing spurious
  `Op::Pop` emissions in script mode.
- **`json_extract` unicode escape handling** — `\uXXXX` sequences inside
  JSON strings no longer cause incorrect bracket balancing.
- **`format()` double-substitution** — named placeholder replacement now
  uses single-pass scanning to prevent values containing `{key}` patterns
  from being re-substituted.
- **Lint builtin list** — derived from VM registration instead of hardcoded
  300-line array that drifted from actual builtins.

## v0.4.26

### Added

- **Implicit pipeline (script mode)** — files without a `pipeline` block now
  execute top-level code directly. Write `println("hello")` without wrapping
  in a pipeline.
- **`in` / `not in` operators** — membership testing for lists, dicts, strings,
  and sets: `if name in users`, `if key not in config`.
- **`url_encode` / `url_decode` builtins** — RFC 3986 percent-encoding for
  building API URLs and decoding query strings.
- **Named format placeholders** — `format("Hello {name}", {name: "world"})`
  in addition to existing positional `{}` placeholders.
- **Enhanced `progress` builtin** — now supports numeric progress and total:
  `progress("indexing", "Processing files", 3, 10)`. Auto-emits progress
  during `agent_loop` iterations in bridge/ACP mode.

### Changed

- **`pi` and `e` are now constants** — use `pi` and `e` directly instead of
  `pi()` and `e()`. **Breaking change**: calling them as functions will error.

### Fixed

- **`json_extract` balanced bracket matching** — extracts the first balanced
  JSON structure instead of spanning from first `{` to last `}`. Fixes
  incorrect extraction from mixed content like `"result: {a: 1}. more {b: 2}"`.

### Documentation

- **New Getting Started guide** with installation, first program, REPL usage.
- **New MCP and ACP Integration guide** covering client/server usage.
- **New CLI Reference** documenting all commands.
- **Restructured docs** — added Getting Started as first page, moved TCO
  to advanced patterns, documented `parallel_settle`, `llm_stream`, cost
  tracking, and all v0.4.26 features.
- **Code snippet overhaul** — all examples use `harn` code fences, `println()`
  for output, and current syntax.

## v0.4.25

### Added

- **H3: Checkpoint & Resume** — comprehensive support for resilient,
  resumable pipelines that survive crashes and restarts.
- **`checkpoint_exists(key)`** — returns `true` if the key is present in
  checkpoint data, even when the stored value is `nil`. More reliable than
  `checkpoint_get(key) == nil` for existence checks.
- **`checkpoint_delete(key)`** — removes a single key from the checkpoint
  store without clearing everything. No-op if the key is absent.
- **`std/checkpoint` module** — importable utilities for the resume pattern:
  - `checkpoint_stage(name, fn)` — runs `fn()` and caches the result; on
    subsequent calls returns the cached value without re-executing `fn`.
    The primary primitive for building idempotent, resumable pipelines.
  - `checkpoint_stage_retry(name, max_retries, fn)` — like `checkpoint_stage`
    but retries `fn()` up to `max_retries` times on failure before
    propagating the error. Cached on first success.

## v0.4.19

### Fixed

- `std/async` module: renamed `deadline` variable to `end_time` — `deadline`
  is a reserved keyword in Harn, so `wait_for`, `retry_with_backoff`, and
  `circuit_call` were all broken at import time
- Fixed `generator_simple.harn` conformance test formatting
- Fixed ACP metadata base path to use project root

### Added

- `parallel_settle` conformance test

## v0.4.18

### Added

- **Generators / coroutines**: Functions using `yield` become generators.
  Calling them returns a generator object; `.next()` produces `{value, done}`.
  Generators work with `for-in` loops for lazy iteration.

  ```harn
  fn fibonacci() {
    var a = 0
    var b = 1
    while true {
      yield a
      let temp = a
      a = b
      b = temp + b
    }
  }
  for n in fibonacci().take(8) { println(n) }
  ```

- **Structured error types**: `ErrorCategory` enum with categories: timeout,
  auth, rate_limit, tool_error, cancelled, not_found, circuit_open, generic.
  Error classification uses HTTP status codes (RFC 9110) and well-known API
  error identifiers from Anthropic/OpenAI.
- **Error builtins**: `error_category(err)`, `throw_error(msg, category)`,
  `is_timeout(err)`, `is_rate_limited(err)`
- **`parallel_settle`**: Like `parallel_map` but wraps each result in
  `Result.Ok/Err` — returns `{results, succeeded, failed}` instead of
  failing on first error
- **Circuit breaker**: `circuit_breaker(name, threshold, reset_ms)`,
  `circuit_check(name)`, `circuit_record_success(name)`,
  `circuit_record_failure(name)`, `circuit_reset(name)`.
  Plus `circuit_call(name, fn)` in `std/async`.
- **Tool retry with backoff**: `agent_loop` now accepts `tool_retries` and
  `tool_backoff_ms` options for automatic tool call retry with exponential
  backoff
- **A2A spec alignment**: Task states now include `rejected`, `input-required`,
  `auth-required` per A2A protocol v0.3. Error codes use standard A2A names
  (`TaskNotFoundError`, `TaskNotCancelableError`, `UnsupportedOperationError`)

### Changed

- Deadline inheritance: child VMs from `spawn`/`parallel` now inherit the
  parent's deadline stack
- Error classification is based on HTTP status codes and documented API error
  types rather than fragile substring matching

## v0.4.17

### Added

- **`harn.toml` check config**: New `[check]` section with `strict` (warnings
  become errors) and `disable_rules` (skip specific lint rules). Example:

  ```toml
  [check]
  strict = true
  disable_rules = ["shadow-variable", "unused-parameter"]
  ```

- **"Did you mean?" suggestions**: Levenshtein-based fuzzy matching for:
  - Linter `undefined-function` rule suggests closest known function
  - Shape validation suggests closest field name on missing fields
- **`harn test --verbose`**: Per-test timing display, slowest-tests summary
- **`catch e {` without parens**: `catch e { ... }` now works alongside
  `catch (e) { ... }` — the parentheses are optional
- **Fixed `std/json` module**: Rewrote `safe_parse`, `merge`, `pick`, `omit`
  to use correct Harn syntax (was using unsupported `catch e` and `{..spread}`)

### Changed

- `TypeDiagnostic` now carries optional `help` text for richer error output
- Conformance tests: 230 total (3 new: catch_no_parens, stdlib_json,
  did_you_mean_shape)

## v0.4.16

### Added

- **Set method syntax**: Sets now support dot-notation methods matching
  lists and dicts. All set operations work as methods:
  `.add()`, `.remove()`, `.contains()`, `.union()`, `.intersect()`,
  `.difference()`, `.symmetric_difference()`, `.is_subset()`,
  `.is_superset()`, `.is_disjoint()`, `.to_list()`, `.map()`,
  `.filter()`, `.any()`, `.all()`, `.count()`, `.empty()`
- **New set builtins**: `set_symmetric_difference()`, `set_is_subset()`,
  `set_is_superset()`, `set_is_disjoint()` (function-style)

## v0.4.15

### Added

- **System introspection builtins**: `username()`, `hostname()`, `platform()`,
  `arch()`, `home_dir()`, `pid()`, `cwd()`, `date_iso()` for building
  dynamic system prompts and understanding execution context
- **Path introspection**: `source_dir()` returns the directory of the
  currently-executing .harn file; `project_root()` walks up to find
  the nearest `harn.toml` (returns nil if not found)
- **`std/async` stdlib module**: `wait_for(timeout_ms, interval_ms, predicate)`,
  `retry_until(max_attempts, predicate)`, `retry_with_backoff(max_attempts,
  base_ms, predicate)` — all return `Result` (Ok on success, Err on timeout)
- **New string methods**: `trim_start()`, `trim_end()`, `lines()`,
  `char_at(index)`, `last_index_of(substr)`, `len()` (method form)
- **New list methods (Ruby-inspired)**: `none(predicate?)`, `every(pred)`
  (alias for `all`), `find_index(pred)`, `first(n?)`, `last(n?)`,
  `partition(pred)`, `group_by(key_fn)`, `chunk(size)` / `each_slice(size)`,
  `min_by(key_fn)`, `max_by(key_fn)`, `compact()`, `each_cons(size)` /
  `sliding_window(size)`, `tally()`

### Fixed

- `index_of()` and `last_index_of()` now return character offsets (not byte
  offsets), consistent with `substring()`, `char_at()`, and `len()`
- Added missing builtins (`shell`, `elapsed`, `timestamp`, `scan_directory`,
  and all new v0.4.15 builtins) to linter's `undefined-function` known list

### Changed

- `all` list method now also responds to `every` and `all?` aliases
- Thread-local source directory tracking for `source_dir()` / `project_root()`

## v0.4.14

### Fixed

- `len()` now returns character count for strings (was byte count),
  consistent with `substring()` which uses character indexing
- `date_format` rejects negative timestamps with an error instead of
  panicking via unsigned integer overflow
- `date_parse` validates month (1-12), day (1-31), hour (0-23),
  minute (0-59), second (0-59) ranges
- `select`/`__select_timeout`/`__select_list` use 1ms sleep instead
  of `yield_now()` busy-loop, reducing CPU usage when no channels are ready
- Thread-local state (LLM budget/cost, trace stack, log level, HTTP mocks)
  is now reset between test runs for proper isolation
- `trace_end` verifies span ID matches before popping the trace stack
- `run_watch` (`harn watch`) now respects `--deny` and `--allow` flags
- `http_mock` URL matching supports multi-`*` glob patterns
  (e.g., `https://api.example.com/*/items/*`)
- Removed unnecessary `.as_str()` in `Rc::from()` calls throughout
  the codebase (~30 occurrences), eliminating intermediate allocations

### Added

- **Definition-site generic checking**: inside generic function bodies,
  method calls on constrained type parameters (`where T: Interface`)
  are validated against the bound interface's methods
- **Runtime interface enforcement**: function parameters typed as
  interfaces are now checked at runtime (not just compile-time)
- **Shape validation for union type fields**: shape annotations like
  `{value: string | int}` now validate the field's type at runtime
- **Undefined function linter rule**: `undefined-function` warns on
  calls to functions not declared in the current file or builtins
- **Multi-file `harn fmt`**: the `fmt` command now accepts multiple
  files and directories (e.g., `harn fmt src/ tests/`)
- `reset_thread_local_state()` public API for test harness isolation
- `scan_directory(path?, pattern?)` builtin for native filesystem enumeration
  with glob support, depth limiting, and mtime tracking
- Real `metadata_stale()` implementation comparing stored structure/content
  hashes against filesystem state (was previously a no-op)
- `metadata_refresh_hashes()` now recomputes and stores structure hashes
- Conformance tests for all fixed bugs

### Changed

- DRY: extracted `ResolvedProvider` helper for shared provider config
  resolution between `stream.rs` and `api.rs`
- Simplified `Makefile` `fmt-harn` target to use directory argument
- SSE streaming (`llm_stream`) refactored to use `ResolvedProvider`

## v0.4.13

### Added

- **Cost tracking**: `llm_cost()`, `llm_session_cost()`, `llm_budget()`,
  `llm_budget_remaining()` builtins for estimating and capping LLM spend
- **Mock HTTP framework**: `http_mock()`, `http_mock_clear()`,
  `http_mock_calls()` for testing pipelines without real HTTP calls
- **Inline evaluation**: `harn run -e 'expression'` for quick one-liners
- **Spread in method calls**: `obj.method(...args)` now works (previously
  only worked in function calls)
- **Provider fallback chains**: `fallback` field in providers.toml for
  automatic retry with a different provider on failure
- **Graceful cancellation**: `cancel_graceful()` and `is_cancelled()`
  builtins for cooperative task shutdown
- Conformance tests for all new features

### Fixed

- Interface satisfaction checking now validates parameter types and return
  types, not just method names and parameter counts
- Generic constraints (`where T: Interface`) now work inside container
  types like `list<T>` and `dict<string, T>`
- Static analysis warns on calls to undefined functions (when receiver
  type is known)

### Changed

- **Code quality**: Split 4 oversized files into focused modules:
  - `stdlib.rs` (3109 lines) -> 17 category modules
  - `llm.rs` (2634 lines) -> 10 sub-modules
  - `harn-lsp/main.rs` (3150 lines) -> 7 modules
  - `harn-cli/main.rs` (1684 lines) -> 5 command modules
- Updated documentation for HTTP builtins (added PUT, PATCH, DELETE),
  provider configuration, cost tracking, and mock HTTP
- Added contributor setup guide and provider config to README

## v0.4.12

### Added

- Data-driven provider configuration via `providers.toml`
- 6 new LLM introspection builtins: `llm_resolve_model`,
  `llm_infer_provider`, `llm_model_tier`, `llm_healthcheck`,
  `llm_providers`, `llm_config`
- Debug logging for provider config loading

## v0.4.11

### Added

- Interfaces with implicit satisfaction (Go-style)
- Generic constraints: `where T: Interface`
- Runtime shape validation for function parameters
- Try-expression: `try { expr }` returns `Result`
- Regex capture groups with named group support
- Spread operator in function calls: `f(...args)`

## v0.4.10

### Added

- `Result` type with `Ok`, `Err`, `unwrap`, `unwrap_or`, `unwrap_err`
- `impl` blocks for attaching methods to structs
- Type narrowing: nil removed from union types after `!= nil` checks
- Stack traces in error messages
- Postfix `?` operator for Result unwrapping

## v0.4.9

### Changed

- Removed bridge `llm_call`/`llm_stream` — native VM handles all LLM calls

## v0.4.8 - v0.4.5

### Added

- Native metadata builtins
- Bridge fixes and conformance tests
- Default function arguments
- `finally` blocks in try/catch
- `select` statement for channel multiplexing
