# Changelog

All notable changes to Harn are documented in this file.

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
- **`harn check` preflight is host-extensible instead of Burin-hostile** —
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
