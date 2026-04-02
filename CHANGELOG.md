# Changelog

All notable changes to Harn are documented in this file.

## v0.5.25

### Changed

- **Text-mode tool calling no longer bakes in IDE-specific tool names** — the
  runtime-owned contract and agent nudge examples now stay generic, and
  positional text-call parsing only infers a parameter name from the live tool
  schema when a tool declares exactly one parameter.
- **Scalar argument parsing remains type-stable in text mode** — floating-point,
  boolean, integer, and `null` values in ` ```call ` blocks continue to round-trip
  as structured JSON values instead of silently degrading into strings.

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
