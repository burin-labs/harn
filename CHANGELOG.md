# Changelog

All notable changes to Harn are documented in this file.

Prior-series highlights (pre-0.6) are condensed at the bottom. Harn had no
external users before 0.6.0, so we intentionally do not preserve the full
per-patch history of the 0.5.x and 0.4.x lines here — consult `git log` for
granular archaeology.

## v0.7.0

**First-class sessions.** The old `transcript_policy` config pattern is gone.
Session lifecycle — open, reset, fork, trim, compact, inject, snapshot, close —
is now driven by explicit imperative builtins. Unknown inputs are hard errors
instead of silent no-ops. The session store is the single source of truth for
session-scoped VM state: transcript history and closure subscribers both live
on the session now.

This is a semver-minor in the 0.x series. It is a breaking change: pipelines
that relied on `transcript_policy` dict semantics, `transcript_id` /
`transcript_metadata` on `llm_call` options, or the opaque `transcript`
option key must migrate.

### Breaking

- **`transcript_policy` removed everywhere it lived.** Deleted on workflow
  graph nodes, worker carry policies, and the `TranscriptPolicy` struct
  itself. The auto-compaction fields that used to live on it (`auto_compact`,
  `compact_threshold`, `tool_output_max_chars`, `compact_strategy`,
  `hard_limit_tokens`, `hard_limit_strategy`) moved to a dedicated
  `AutoCompactPolicy` struct under `node.auto_compact`. The `visibility`
  field was split out to a direct `node.output_visibility: string | nil`.
- **`workflow_set_transcript_policy` removed.** Replaced by
  `workflow_set_auto_compact` and `workflow_set_output_visibility`.
- **`mode: "reset" | "fork"` lifecycle dict is gone.** Call
  `agent_session_reset(id)` or `agent_session_fork(src)` explicitly.
- **`transcript` option key on `llm_call` / `agent_loop` now hard-errors.**
  Pass `session_id: id` — the loop loads prior messages from the session
  store as a prefix, and persists the final transcript back on exit.
- **`LlmCallOptions::transcript_id` and `transcript_metadata` removed.**
  Session id subsumes both. `transcript_summary` stays (per-call summary
  injection for mid-loop compaction output).
- **`CLOSURE_SUBSCRIBERS` thread-local in `agent_events.rs` removed.**
  Subscribers now live on `SessionState.subscribers` in
  `crate::agent_sessions`. `agent_subscribe(id, cb)` opens the session
  lazily and appends. `clear_session_sinks` no longer evicts the session
  itself — it only clears external ACP-style sinks.
- **`execute_stage_node` no longer takes a `transcript: Option<VmValue>`
  param.** Stages read prior transcripts from the session store instead,
  via the stage's resolved `session_id`.
- **Unknown `agent_session_compact` option keys, a missing `role` on
  `agent_session_inject`, a negative `keep_last`, and lifecycle verbs
  called against an unknown id all raise** `VmError::Thrown`. Previously
  many of these were silent pass-throughs.
- **Workflow `input_contract.require_transcript` now checks the session
  store** (via the stage's `model_policy.session_id`) rather than the
  ambient threaded transcript dict.

### Added

- **Ten new builtins** in `crate::stdlib::agent_sessions`:
  `agent_session_open`, `_exists`, `_length`, `_snapshot`, `_reset`,
  `_fork`, `_close`, `_trim`, `_compact`, `_inject`. Fully documented in
  `docs/src/sessions.md`, exercised by new conformance tests
  `agent_sessions_basic.harn` and `agent_sessions_fork.harn`, and
  covered by 12 Rust integration tests in
  `crates/harn-vm/tests/agent_sessions.rs`.
- **`workflow_set_auto_compact(graph, node_id, policy)`** and
  **`workflow_set_output_visibility(graph, node_id, visibility)`** replace
  the single `workflow_set_transcript_policy`.
- **`crate::agent_sessions` module** — public per-thread session store with
  LRU eviction (default 128 sessions per VM), subscriber fanout, transcript
  round-trip for the agent loop.
- **`redact_transcript_visibility`** lifted to a public helper in
  `crate::orchestration::policy`, reusable from workflow stages and any
  embedder that wants to filter a transcript by visibility.

### Changed

- `agent_loop` with a caller-supplied `session_id` now loads prior
  messages from the session store as a prefix before running, and
  persists the final transcript back on exit. Calls without a
  `session_id` (or with an empty string) mint an anonymous id and do
  not touch the store — preserving the one-shot call shape.
- Workflow stage execution derives its `session_id` from the node's
  `model_policy.session_id`; two stages sharing an id share a
  conversation automatically.

## v0.6.3

Maintenance release focused on **comment hygiene and dependency freshness**.
No user-visible language or runtime changes — behavior, builtins, and the
conformance suite are unchanged (still 419 tests, 546 Rust tests).

### Changed

- **Comment cleanup across the workspace** — 114 files, ~2,100 net lines
  removed. Pruned section-header banners, drift narration from prior
  refactors, step-by-step breadcrumbs that paraphrased function bodies, and
  doc comments that just restated signatures. Preserved comments that document
  non-obvious invariants, protocol/spec compliance (JSON-RPC, MCP, ACP),
  provider-specific quirks (Anthropic, OpenAI, Google, Ollama, Together,
  OpenRouter), and real footguns.
- **`rand` upgraded from 0.8 to 0.9** — migrated deprecated
  `thread_rng`/`gen`/`gen_range` call sites to the renamed `rng`/`random`/
  `random_range` APIs.
- **`sha2` upgraded from 0.10 to 0.11** — unified the `sha2::*` and `md5`
  hash builtins under a single hex-encoding macro now that both pin
  `digest 0.11`.
- **`futures` pin relaxed** from the exact `0.3.32` to the semver-compatible
  `0.3`, matching the rest of the workspace's version-range style.
- `cargo update` brought patch bumps for tokio, rustls, hashbrown, indexmap,
  wasm-bindgen, and several transitive deps.

## v0.6.2

Polish patch focused on **agent-loop correctness and error-handling
depth**. Restructures error classification end-to-end (new
`ErrorCategory` variants, HTTP-status mapping fixes, category-first
retry classifier, RFC-compliant retry-after parsing), fixes several
silent-failure modes in the agent loop, hardens the streaming
transport against pathological slow-drip providers, and unifies a
handful of CLI and observability rough edges. Conformance suite goes
from 418 → 419 tests; Rust tests from 530 → 546.

### Breaking

- **`ErrorCategory` gains 4 variants** — `Overloaded`, `ServerError`,
  `TransientNetwork`, `SchemaValidation`. Non-exhaustive matches on
  `ErrorCategory` at the FFI/host-consumer boundary must handle the
  new variants (or add a wildcard arm). In-tree exhaustive sites were
  updated in this commit.
- **HTTP status → category mapping corrected.** 503 is now
  `Overloaded` (not `RateLimit` — 503 is an overload/shedding signal,
  not a quota hit). 500 and 502 are now `ServerError` (were falling
  through to `Generic`). 529 is `Overloaded`. 504 stays `Timeout`.
  Hosts that pattern-match on `rate_limit` will no longer see 503s
  there; match on `overloaded` or use `ErrorCategory::is_transient()`
  for a retry decision.
- **Anthropic `overloaded_error` string matches `Overloaded`**, not
  `RateLimit`. Same rationale as the status-code fix.
- **`agent_loop` terminal `status` distinguishes budget exhaustion.**
  When the loop completes `max_iterations` without any natural break,
  `status` is now `"budget_exhausted"` (previously the same `"done"`
  used for natural termination). Daemon loops in the same condition
  report `"budget_exhausted"` instead of being silently relabeled as
  `"idle"`. The conformance `agent_daemon_mode` fixture was updated
  to assert the new shape; host consumers that keyed off `"done"` to
  detect "agent is finished" should add `"budget_exhausted"` to the
  list (the loop ran out of rope, not out of work).

### Added

- **`ErrorCategory::is_transient()`** — authoritative retry-worthy
  predicate. Returns true for `Timeout | RateLimit | Overloaded |
  ServerError | TransientNetwork`.
- **`idle_watchdog_attempts` agent_loop option** — opt-in watchdog
  that terminates a daemon with `status = "watchdog"` after N
  consecutive idle ticks returning no wake reason. Guards against a
  misconfigured daemon (bridge never signals, no timer, no watch
  paths) hanging the session silently.
- **Three internal `AgentEvent` variants** — `BudgetExhausted`,
  `LoopStuck`, `DaemonWatchdogTripped`. Hosts subscribing to the
  event stream get parity with other loop-terminal signals.
- **`cache_hit` boolean in provider-response transcript entries** so
  consumers don't reverse-engineer it from `cache_read_tokens`.
- **RFC 7231 HTTP-date support in retry-after parsing.** The previous
  implementation only handled integer-seconds form and silently
  ignored the date form that major providers emit. Numeric seconds
  are clamped to `[0, 60_000]` ms so a misbehaving provider asking
  for a 10-minute sleep doesn't freeze the caller.
- **Streaming overall deadline.** `vm_stream_llm` now enforces a
  30-minute default overall budget (or the caller's `timeout`) in
  addition to the per-chunk idle timeout, so a provider dribbling
  bytes just under the idle threshold can't hold a stream open
  forever.
- **16 new unit tests** for retry classification and retry-after
  parsing; one new conformance case (`agent_budget_exhausted`).

### Fixed

- **Partial tool-call parse-error feedback was silently swallowed**
  when a batch mixed valid and malformed calls. The feedback gate
  was `calls.is_empty() && !tool_parse_errors.is_empty()`; it is now
  `!tool_parse_errors.is_empty()` with a clarifying note that the
  other calls in the turn dispatched successfully. Previously the
  model saw an apparent random failure to follow instructions.
- **`-e` eval leaked / raced on the temp file.** Fixed path was
  `$TMPDIR/__harn_eval__.harn`, so concurrent invocations clobbered
  each other and a panic in `run_file` left the file behind. Now
  uses `tempfile::NamedTempFile` with Drop-guarded cleanup.
- **`retry-after: <seconds>` with awkward type ascription cleaned up**
  (stylistic; no behavior change on that path).
- **`stop_after_successful_tools` unknown names are now flagged.** A
  warning names the unknown tool(s) at loop start; the option is
  still tolerated (forward-compat) but the user sees why their stop
  condition never fires.
- **Schema-validation error message now includes an actionable hint.**
  If `schema_retries` was 0, the error points at the option; if it
  was > 0 and got exhausted, the error says so plainly. The error is
  also now a `CategorizedError { category: SchemaValidation, .. }`
  rather than an opaque `Thrown(String)`.

### Changed

- **`is_retryable_llm_error` is category-first.** Structured
  `CategorizedError`s route through `is_transient()`. String-shaped
  errors first consult the shared `classify_error_message` machinery
  in `value.rs` so HTTP status codes and well-known provider
  identifiers are interpreted consistently with the rest of the VM,
  then fall back to a small substring list for shapes that carry no
  status (network failure phrases).
- **Micro-allocations swept:** `.to_string_lossy().to_string()` →
  `.to_string_lossy().into_owned()` across 14 files. Identical
  semantics, one fewer allocation on the owned variant.

### Internal

- **Dead-code/lint sweep.** The `#[allow(clippy::
  arc_with_non_send_sync)]` in `stdlib/concurrency.rs` gains a
  why-comment anchoring it to the documented single-threaded
  LocalSet invariant. The `MultiSink::handle_event` clone-then-
  iterate pattern is now documented as deliberate deadlock
  avoidance rather than an obvious "optimization" candidate.
- **Docs reconciled.** `docs/src/llm-and-agents.md` now lists the
  full `status` state space and documents the new
  `idle_watchdog_attempts` option.

Rust tests: harn-vm lib 546/546, harn-cli 124/124.
Conformance: 419/419.

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
