# Changelog

All notable changes to Harn are documented in this file.

Prior-series highlights (pre-0.6) are condensed at the bottom. Harn had no
external users before 0.6.0, so we intentionally do not preserve the full
per-patch history of the 0.5.x and 0.4.x lines here — consult `git log` for
granular archaeology.

## v0.6.1

Patch release. Completes the WS-6 agent/mod.rs modularization started in
the 0.6.0 line: `llm/agent/mod.rs` was a 1734-LOC hub carrying most of
the per-iteration turn loop inline. This release finishes the split
along phase seams so the orchestrator reads top-to-bottom as four named
phases.

### Internal

- **`llm/agent/mod.rs` is now a ~260-LOC thin orchestrator.** The turn
  loop body expands to four phase calls — `turn_preflight`, `llm_call`,
  `tool_dispatch`, `post_turn` — with an `IterationOutcome::{Continue,
  Break}` match returned by `post_turn` to drive outer-loop control
  flow.
- **`agent/llm_call.rs`** (new, ~410 LOC) owns the provider call,
  tagged-prose parsing, parse/protocol/sentinel feedback injection, and
  ledger-tool interception.
- **`agent/tool_dispatch.rs`** (new, ~840 LOC) owns the assistant-turn
  history append, read-only parallel pre-fetch, and the per-tool
  dispatch pipeline (parse-error rejection, policy enforcement,
  declarative + host approval via `session/request_permission`,
  pre/post hooks, arg validation, loop-detect, replay/cached/fresh
  dispatch, tracing spans, `ToolCall` / `ToolCallUpdate` events,
  transcript events, tool-result message append).
- **`agent/post_turn.rs`** (new, ~490 LOC) owns both the tool-call
  post-processing path (finish_step_messages, consecutive_single_tool,
  successful_tools_used, `TurnEnd` emit, `stop_after_successful_tools`,
  optional `post_turn_callback`, auto-compaction, parse_error feedback,
  sentinel_hit break) and the text-only path (assistant-history append,
  sentinel break, parse_error continue, daemon idle-wait with
  message/resume/watch/timer wake sources, max_nudges stuck detection,
  action-turn nudge).
- **Dead code swept.** `ToolDispatchResult.rejection_followups` was
  never pushed to; dropped the field and its dead-branch guard in the
  orchestrator.

No behavior change: harn-vm lib 530/530, harn-cli 124/124 green at
every commit.

## v0.6.0

Major release that establishes Harn's **lazy iterator protocol** as a
first-class language feature, completes the **coding-agnostic agent
substrate** the 0.5 series had been converging toward, and finishes a
sweeping internal **modularization pass** across the VM, tools, ACP, and
agent crates. Doc-comment syntax migrates from `///` to `/** ... */`
canonically, and the formatter + linter gain a broad set of autofixes
that align code with the new conventions automatically.

### Added

- **Lazy iterator protocol (`Iter<T>`)** — new `VmValue::Iter` variant with
  a single-pass iteration contract, type-checked `Iter<T>` generics, and
  a full combinator surface:
  - **Sources**: `range(start, stop, step?)` builtin, `Range` values now
    implement the iterator source protocol, `.iter()` on collections.
  - **Transformers**: `map`, `filter`, `flat_map`, `take`, `skip`,
    `take_while`, `skip_while`, `zip`, `enumerate`, `chain`, `chunks`,
    `windows`.
  - **Sinks**: `collect`, `reduce`, `fold`, `sum`, `count`, `min`, `max`,
    `any`, `all`, `first`, `last`, `for_each`, `print`.
  - Conformance coverage across sources, snapshot semantics, single-pass
    exhaustion, and streaming print.
  - Python-style `for` iteration and the new inclusive `to` keyword with
    optional trailing `exclusive` replace the old `thru` / `upto` pair.
- **`VmValue::Pair<K,V>` with for-loop destructuring** — `for (k, v) in
  dict` and `for (i, x) in iter.enumerate()` both desugar through a
  first-class pair value that type-checks end-to-end.
- **`eager-collection-conversion` lint** — with autofix. Flags
  `to_list`/`to_dict`/`to_set` calls on lazy iterators whose result is
  immediately re-iterated, steering code toward the streaming form.
- **Formatter / linter autofixes** — six new `harn lint --fix` rules now
  cover: trailing commas, import ordering, blank lines between
  top-level items, optional file-header banners, legacy `///`
  doc comments, and eager collection conversion. The formatter
  canonicalizes section-header comment blocks and enforces blank lines
  between top-level items.
- **`harn.toml` project config** — the CLI now walks upward (bounded at
  git roots) to locate a project manifest and applies its `fmt` / `lint`
  options. Both `snake_case` and `kebab-case` keys are accepted.
- **Canonical doc-comment syntax** — `/** ... */` is now the canonical
  harndoc form. The lexer tags `///` and `/**` as distinct tokens, the
  formatter and a `legacy-doc-comment` lint autofix migrate existing
  code, and `missing-harndoc` now requires the `/**` form.

### Changed

- **Agent substrate is coding-agnostic.** The VM core no longer carries
  coding-specific knowledge; the agent loop communicates through a
  `ToolAnnotations` + `AgentEvent` event stream, replacing the earlier
  ad-hoc callback hooks. The ACP server now speaks canonical
  `SessionUpdate` variants end-to-end and the legacy custom `tool/*`
  bridge methods are retired.
- **Event substrate hardening.** Session event sinks use RAII for
  deterministic lifecycle, subscriber errors are logged instead of
  silently dropped, and the happy/sad paths are covered by new tests.
- **Inclusive range syntax.** `a to b` is inclusive; add a trailing
  `exclusive` keyword for half-open ranges. The older `thru` / `upto`
  forms are removed from the lexer, parser, spec, and grammar.
- **Parser and runtime error messages** — 10–15 high-frequency
  diagnostics were tightened for clarity and actionability.
- **Internal modularization.** Large single-file modules were split
  into focused submodules without changing the public surface: `agent`
  (helpers, state, finalize, turn_preflight, tests), `tools` (parse,
  handle_local, ts_value_parser, tests), `helpers` (options),
  `orchestration` (tests), `policy` (types), `acp` (events, io,
  builtins, execute).

### Fixed

- **`VmRange` overflow hardening.** Range boundaries near `i64::MAX` /
  `i64::MIN` no longer panic.
- **No `RefCell` borrow is held across iterator await points**,
  eliminating a class of runtime borrow-panic regressions that could
  trigger under concurrent iterator sinks.
- **Formatter / conformance fixes** — a handful of pre-existing
  formatter and conformance bugs surfaced by the iterator and
  agent-substrate work are resolved.

### Docs / grammar

- Tree-sitter artifacts regenerated, `harn-keywords.js` synced from the
  live lexer + stdlib, and the language spec + quickref updated to
  describe the iterator protocol, the new range syntax, canonical
  doc-comment form, and the agent-substrate event model.

## v0.5 series (0.5.0 – 0.5.83)

The 0.5 line was Harn's "language and runtime fill-in" phase. Grouped
themes (see `git log` for the per-patch detail that previously lived
here):

- **Language:** generics foundations (generic structs, enums, interface
  associated types), strict types mode with schema-aware `llm_call`
  inference, exhaustive match with guards, `defer`, unified `parallel`
  syntax (`parallel each` / `parallel settle` / `parallel N`), nil-aware
  `??` inference, destructuring with defaults, rest parameters, raw
  string literals, triple-quoted interpolation, first-class `**`
  exponentiation, `never` bottom type + `unreachable()`, typed `catch`
  variables, stricter arithmetic typing, dict `+` merge, `string * int`
  repetition, type narrowing via `type_of`.
- **Orchestration runtime:** delegated workers (`spawn_agent`,
  `send_input`, `wait_agent`, `close_agent`, `list_agents`), worker
  lifecycle events and lineage, delegated workflow stages, workflow
  retry/backoff, stage-level timeouts, `ToolApprovalPolicy` as a
  load-bearing gating primitive, `agent_loop` turn policies,
  `post_turn_callback`, `require_successful_tools`,
  `stop_after_successful_tools`, per-worker permission scoping,
  parallel workflow map execution, daemon lifecycle persistence.
- **LLM surface:** `provider: "auto"` routing, `schema_retries` +
  `schema_retry_nudge`, `llm_retries` default of 2, `llm_usage()`,
  reasoning-content support, silent-completion detection, configurable
  mock LLM, structured output extraction, `llm_mock`, append-only
  transcript event stream, `transcript_stats`,
  `transcript_events_by_kind`, `user_visible` flag on bridge
  notifications, Ollama `think: false` default, Gemma `tool_code:`
  parser fallback, text-mode tagged-protocol hardening (heredocs, bare
  calls, angle-wrapped calls).
- **Tooling:** autofix infrastructure and the first wave of
  `harn lint --fix` rules, LSP formatting + code actions, inlay hints,
  type-aware dot completions, VS Code debugger + snippets, project
  templates (`harn new`), `harn bench` / `harn viz`, OpenTelemetry
  export behind the `otel` feature flag, Dependabot, portal build
  verification wired into `make all`.
- **Protocols:** ACP session lifecycle, MCP server at protocol version
  `2025-11-25`, A2A server at `v1.0.0`, `jsonrpc` helper module,
  machine-readable host contracts (`harn contracts ...`), explicit
  runtime path builtins (`execution_root`, etc.).
- **Schema / validation:** unified runtime schema engine,
  `schema_is` / `schema_check` / `schema_parse`, `std/schema` module,
  tool declarations exposing JSON Schema metadata, `untyped-dict-access`
  lint.
- **Stdlib additions:** `yaml_parse` / `yaml_stringify`, statistical
  and vector helpers in `std/math`, `regex_replace_all` alias,
  `eval_metric` / `eval_metrics`, `md5`, structured agent trace events,
  eval suite manifests + `harn eval`.
- **Fixes and quality:** runtime hot-path allocation reductions,
  release-mode optimization tightening, formatter grouping
  preservation, LSP stability fixes, conformance regex error matching
  and glob filters, diagnostic flakiness, workflow retry data
  preservation, nested agent loop permission ceilings.

## v0.4 series (0.4.5 – 0.4.32)

The 0.4 line established Harn's core language and runtime:

- **Language:** `Result` with `Ok` / `Err`, postfix `?`, `impl` blocks,
  interfaces with implicit satisfaction and `where T: Interface`
  constraints, runtime shape validation, try-expressions, regex capture
  groups with named groups, spread in calls, default function
  arguments, `finally`, `select`, native metadata builtins.
- **Providers / LLM:** data-driven `providers.toml`, LLM introspection
  builtins (`llm_resolve_model`, `llm_infer_provider`, `llm_model_tier`,
  `llm_healthcheck`, `llm_providers`, `llm_config`), native VM-owned
  `llm_call` replacing the bridge path.
- **Orchestration:** eval suite manifests (`eval_suite_manifest`,
  `eval_suite_run`, `harn eval`), host artifact helpers for workspace
  files, snapshots, selections, command results, verification output,
  unified diffs, git diffs, review items, accept/reject decisions.
- **Code quality:** the initial modularization pass that split the
  monolithic `stdlib.rs`, `llm.rs`, `harn-lsp/main.rs`, and
  `harn-cli/main.rs` into focused submodules.
