# Changelog

All notable changes to Harn are documented in this file.

Prior-series highlights (pre-0.6) are condensed at the bottom. Harn had no
external users before 0.6.0, so we intentionally do not preserve the full
per-patch history of the 0.5.x and 0.4.x lines here — consult `git log` for
granular archaeology.

## Unreleased

### Tests

- **CLI regression for the `harn lint <dir>` / `harn check --workspace`
  Linux hang (#748).** The path-spelling explosion in
  `harn_modules::build()` was fixed in #93 (`harn-modules` unit test
  `cross_directory_cycle_does_not_explode_module_count`), but burin-code
  CI still ran a per-file `xargs -n1 harn lint` workaround because the
  fix only had unit-level coverage. Adds
  `lint_and_check_complete_on_large_cross_directory_cycle_workspace` in
  `crates/harn-cli/tests/check_cli.rs`, which builds a 24-file pipeline
  tree across four sibling directories with relative cross-directory
  imports — the exact pattern that triggered the OOM-kill on Linux —
  and asserts both `harn lint <dir>` and `harn check --workspace`
  complete inside a 60 s budget through the CLI binary. Verified to
  fail (test process hangs past 60 s) when the canonicalize-before-seen
  block in `crates/harn-modules/src/lib.rs` is reverted, and to pass
  (sub-second per command) with the fix in place.

### Added

- **`pub import` re-exports for facade modules (#740).** Prefixing any
  `import` with `pub` now re-exports the imported symbols as part of the
  importing module's public surface:
  `pub import "module"` re-exports every public name; `pub import { foo,
  bar } from "module"` re-exports only the listed names. Re-exports
  compose across facade chains, so a `mod.harn` can be the stable public
  entry point while implementation shards move freely behind it. `harn
  check` reports a re-export conflict when two `pub import`s contribute
  the same name from different sources or shadow a local `pub`
  declaration. Editor go-to-definition follows re-export chains to the
  originating declaration. Spec, modules guide, tree-sitter grammar, and
  conformance fixtures (selective, wildcard, chained, duplicate) updated
  to match.

## v0.7.43

### Added

- **Ship Captain persona v0 (#585).** Adds the checked-in
  `personas/ship_captain` pack and enriches `harn flow ship watch` so the
  Phase 0 command groups stored atoms into intents, discovers predicate gates,
  persists a local shipping receipt, and emits an approval-gated mock PR
  receipt with the required eval-pack hooks.
- **Crystallization candidate bundle (#746).** Added the stable
  `harn.crystallization.candidate.bundle` directory layout
  (`candidate.json`, `workflow.harn`, `report.json`, `harn.eval.toml`, and
  a redacted `fixtures/` tree) that Harn Cloud and other downstream
  importers can consume without bespoke glue. `harn crystallize
  --bundle BUNDLE_DIR` emits the bundle, `harn crystallize validate
  BUNDLE_DIR` smoke-checks it (schema marker, required files, redaction,
  logical-only secret ids), and `harn crystallize shadow BUNDLE_DIR`
  re-runs the deterministic shadow comparison from the bundle's redacted
  fixtures with no live side effects. Bundle redaction scrubs sensitive
  keys (`token`, `secret`, `password`, `api_key`, `authorization`,
  `cookie`) and secret-shaped values (`sk-…`, `ghp_…`, `xoxb-…`,
  `AKIA…`, long credential-shaped runs) before fixtures are written.
- **Scoped host/LLM mock fixtures (#745).** `std/testing` now ships
  `with_host_mocks(mocks, body)`, `with_llm_mocks(mocks, body)`, and
  `with_mocks({host_mocks, llm_mocks}, body)` helpers that snapshot the
  current host and LLM mock state, register the supplied fixtures, run the
  body, and restore the prior state on exit — including when the body
  throws. Nested scopes stack cleanly. New `host_mock_push_scope` /
  `host_mock_pop_scope` and `llm_mock_push_scope` / `llm_mock_pop_scope`
  builtins back the helpers and are usable directly when a scope outlives
  a single closure.
- **Flow predicate-language design record (#584).** Added
  `docs/src/flow-predicates.md` with explicit decisions for predicate budget
  semantics, bootstrap signing, semantic predicate determinism, and
  cross-directory slice composition, plus concrete implementation follow-ups
  for the remaining Flow predicate work.
- **Delegated worker transcript carry policies (#700).** `spawn_agent(...)`
  and background `sub_agent_run(...)` workers now persist explicit
  `carry.transcript_mode` semantics: `inherit`, `fork`, `reset`, and
  `compact`. Worker snapshots round-trip the selected mode, compact mode
  reduces persisted carried transcripts while preserving non-message events,
  and parent-facing `worker_result` artifacts now keep compact payloads that
  omit nested full transcripts/artifact lists by default.
- **Persona value-ledger events (#715).** The persona runtime now exposes a
  public `PersonaValueEvent`/`PersonaValueEventKind` contract and an
  RAII-scoped `PersonaValueSink` subscription hook for cloud or self-hosted
  ledger consumers. Runtime run boundaries also persist `persona.value.*`
  events into the existing persona runtime topic, including deterministic
  execution savings and frontier escalation paid-cost deltas.
- **Fixer persona v0 for Flow remediation (#587).** Adds the
  `invariant.blocked_with_remediation` surface, a remediation-bearing
  invariant result, a `harn-vm` Fixer helper that re-signs suggested atoms as
  auditable Fixer atoms and derives a follow-up slice, plus the checked-in
  `personas/fixer` role manifest.
- **Flow predicate composition and replay audit (#571, #582, #583, #584).**
  Flow predicate discovery now pins content hashes that include
  `@archivist(...)` metadata, keeps parent and child predicates applicable for
  hierarchical composition, and exposes conservative stricter-child composition
  checks. `harn flow replay-audit` compares historical slice predicate pins
  with the current `invariants.harn` set, while `harn flow ship watch` and
  `harn flow archivist scan` provide the Phase 0 Ship Captain and Archivist
  command surfaces for shadow-mode workflows.
- **Flow `InvariantResult` graded-verdict types and Harn bindings (#581).**
  Predicates now return a structured `InvariantResult { verdict, evidence,
  remediation, confidence }` value where `verdict` grades as `Allow`, `Warn`,
  `Block`, or `RequireApproval` (routing to a specific `Principal` or
  `Role`). Evidence items cover `AtomPointer`, `MetadataPath`,
  `TranscriptExcerpt`, and `ExternalCitation`. Matching Harn-side builtins let
  `.harn` predicates produce these values idiomatically.
- **First-class worker lifecycle events on ACP and A2A (#703).** Adds
  two new typed `WorkerEvent` variants (`WorkerProgressed`,
  `WorkerWaitingForInput`) and surfaces every worker lifecycle
  transition through a canonical `AgentEvent::WorkerUpdate`. The ACP
  adapter now translates worker updates into `session/update`
  notifications with a `worker_update` discriminator carrying the
  typed event name, status string, terminal hint, full bridge metadata,
  and audit-session record. The A2A adapter registers a per-task
  `AgentEventSink` that publishes `worker_update` events onto the
  task's SSE / replay event stream, scoped via a new
  `agent_session_id` field on `CallRequest` so the sink delivers only
  to the originating task. Retriggerable workers now emit
  `WaitingForInput` instead of going silent when a cycle ends, and
  `worker_trigger` emits `Progressed` on resume so observers see the
  re-arming transition. Bridge protocol docs document the new
  lifecycle states and wire shape.
- **Streaming partial native tool-call arguments (#693).** Anthropic
  `input_json_delta` and OpenAI `tool_calls[].function.arguments` deltas
  now drive `AgentEvent::ToolCall(Pending)` + a coalesced sequence of
  `AgentEvent::ToolCallUpdate(Pending, raw_input | raw_input_partial)`
  events from the SSE transport, so ACP/A2A clients can render tool
  arguments live ("calling search_web…", "edit path=foo.swift,
  replace=…") instead of staring at a black box until `content_block_stop`.
  When the streamed bytes still parse as JSON (strict or after a
  permissive recovery pass closes dangling brackets/strings) the wire
  carries `raw_input`; if neither parse succeeds the raw concatenated
  bytes go on `raw_input_partial`. Updates are coalesced to one per
  ~50 ms per tool block to avoid event-storm pressure on slow clients.
  The `tool_dispatch.rs` lifecycle (`Pending → InProgress →
  Completed/Failed`) still owns the canonical end-of-call state with
  the fully-parsed args.
- **Mutation-session audit on tool_call ACP events (#699).** Both
  `tool_call` and `tool_call_update` `session/update` notifications now
  carry an optional `audit` field mirroring the active
  `MutationSessionRecord` (session id, run id, worker id, mutation
  scope, approval policy). Hosts can now group every write-capable
  dispatch into the right mutation session straight off the canonical
  ACP stream — no more correlating against the
  `session/request_permission.mutation` payload (which only fires on
  approval-gated calls) or the `worker_update.audit` mirror. The field
  is omitted when no mutation session is installed, so existing clients
  see no wire change.
- **Flow `invariants.harn` discovery + provenance attributes (#579).**
  Adds the discovery walker that mirrors `metadata_resolve` semantics
  (root-to-leaf, stricter-child overrides) for per-directory
  `invariants.harn` Flow predicate files, the structured
  `@archivist(evidence: [...], confidence, source_date,
  coverage_examples)` provenance attribute, and the advisory
  `@retroactive` flag. The typechecker now warns when a bare
  `@invariant` is missing exactly one of `@deterministic`/`@semantic` or
  is missing `@archivist(...)`, and the LSP surfaces the full attribute
  block on hover so the function declaration stays the single source of
  truth. Attribute argument syntax also accepts list literals and
  multi-line forms so provenance blocks can carry rich evidence.

### Fixed

- **Decouple GitHub release from the multi-arch container publish.** The
  `release` job in `build-release-binaries.yml` no longer waits on the
  `Publish container` job, so the release tag and binary tarballs/zip
  attach as soon as the build matrix completes. The container still
  gates on the build matrix and publishes on its own schedule. This
  shaves the container build's wall-clock (~5–15 min) off the
  end-to-end release latency, which previously held up
  `fetch-harn.sh`-driven downstream consumers.
- **Stabilize integration tests under full nextest load.**
  `PROCESS_READY_TIMEOUT` in `harn_serve_mcp_cli` and `mcp_server_cli` was
  raised from 15s to 60s after observing 30–40s cold-starts of the debug
  `harn` binary when nextest fans out across the full workspace. The
  in-process healthcheck stub server in `llm::healthcheck` now also runs
  with a 30s accept/read/write deadline so the test thread doesn't trip
  when starved of CPU. Protocol/logic budgets remain tight so regressions
  still surface quickly.

## v0.7.42

### Added

- **Flow predicate executor (#704).** Added predicate-mode Flow execution
  with runtime attribute recognition for predicate declarations, pipelines,
  and ACP skill surfaces. The release also tightens attribute placement
  diagnostics so valid runtime annotations pass conformance while invalid
  placements still warn.
- **Workflow crystallization substrate (#713).** Added the substrate for
  persisting and replaying crystallized workflows, including durable atom
  storage and operator-facing hooks that let Harn turn repeated orchestration
  patterns into reusable workflow state.
- **Alternate forge connector catalog (#712).** Registers the pure-Harn
  Forgejo, Gitea, Bitbucket, SourceHut, and SVN connector package repos in
  the connector catalog and generated trigger quick reference alongside the
  existing forge integrations.

### Changed

- **Embedded orchestrator MCP serving (#709).** `harn orchestrator serve`
  now exposes the orchestrator MCP surface from the deployable listener so
  trigger, queue, replay, inspection, trust-query, and secret-scan tools can
  be served behind the orchestrator auth boundary.
- **Connector and durability cleanup (#706, #707, #708, #710, #711).**
  Adds the SQLite Flow atom store, closes connector epic documentation gaps,
  hardens event-log durability paths, completes the pure-Harn connector pivot
  guardrails, and strengthens orchestrator deploy secret synchronization.

## v0.7.41

### Added

- **Connector epic closure docs (#151).** Added a connector architecture
  status page that records the current core/external-package boundary,
  maps the old Rust-provider library epic to the shipped core substrate, and
  points provider-specific work at the pure-Harn package repos plus #350/#446.
  Updated generic webhook docs to describe the current route-backed listener,
  raw-body `TriggerEvent` path, and durable inbox dedupe instead of the old
  O-02/T-09 deferrals.
- **Embedded orchestrator MCP endpoint (#152).** `harn orchestrator serve`
  now accepts `--mcp` to mount the existing orchestrator MCP HTTP server
  on the deployable listener, with configurable Streamable HTTP and
  legacy SSE paths. The embedded surface requires
  `HARN_ORCHESTRATOR_API_KEYS` so trigger fire/list/replay, queue,
  DLQ, inspect, trust-query, and secret-scan tools remain behind the
  same bearer or `x-api-key` auth used by the orchestrator runtime.
- **Alternate git forge connector catalog (#305).** Registers the
  first-party pure-Harn Forgejo, Gitea, Bitbucket, SourceHut, and SVN
  connector package repos alongside GitHub and GitLab in the connector
  catalog, and includes them in the generated trigger quick reference
  package table.
- **Streaming text-mode tool-call candidate events (#692).** While the
  model is still writing a `<tool_call>` body or a bare `name({...})`
  call, the runtime now emits a candidate-lifecycle stream so ACP
  clients can render an in-flight chip instead of waiting for the full
  response. Adds a `parsing` boolean on both `tool_call` and
  `tool_call_update`: `parsing: true` opens the chip when a candidate
  shape is detected at line start (or inside `<tool_call>`); the
  terminal `tool_call_update { parsing: false }` either promotes
  (`status: pending` with the parsed `rawInput`) or aborts
  (`status: failed`, new `error_category: parse_aborted`) once the
  args resolve. The detector respects markdown code-fence context so
  `function(x)` snippets inside a triple-backtick block do not trigger
  spurious candidate events. Tool dispatch IDs are unchanged — this is
  purely additive observability layered ahead of the post-stream
  parser.
- **Tool-call timing on ACP `tool_call_update` (#689).** Terminal
  `tool_call_update` events now carry `durationMs` (the parse-to-finish
  total — model emits the call → tool result is appended) and
  `executionDurationMs` (only the inner host/builtin/MCP dispatch
  window). Both fields are absent on intermediate `pending` /
  `in_progress` updates so older clients see no shape change. ACP
  clients (Burin CLI/TUI/IDE) can render duration without measuring
  wall-clock time themselves.
- **Per-loop `AgentEvent` sink wired through `AgentLoopConfig`.**
  `AgentLoopConfig.event_sink` was previously a dead field; the loop
  now installs the sink as a thread-local for the duration of the run
  via a new `LoopSinkGuard`. Per-loop sinks fan out alongside the
  global session-keyed registry, immune to concurrent
  `reset_all_sinks` / `reset_thread_local_state` calls. Lets host
  embedders observe a single loop's events without contending on the
  shared registry.
- **Pure-Harn connector pivot closure guard (#350).** Centralizes the
  deprecated Rust compatibility provider list for GitHub, Slack, Linear,
  and Notion and adds a VM regression test that permits only core runtime
  providers or those explicit compatibility shims to remain Rust builtin
  connectors. The migration and connector reference docs now describe
  #446 as completed core groundwork under the #350 pivot, with new
  service connector work directed to pure-Harn packages.

## v0.7.40

### Added

- **Structured `error_category` on `tool_call_update` events (#690).**
  Adds `ToolCallErrorCategory` (snake_case wire enum:
  `schema_validation`, `tool_error`, `mcp_server_error`,
  `host_bridge_error`, `permission_denied`, `rejected_loop`, `timeout`,
  `network`, `cancelled`, `unknown`) on `AgentEvent::ToolCallUpdate`
  alongside the existing free-form `error` string. The dispatch loop
  now categorizes every failure path — schema validation, parse-error
  short-circuit, policy denial, dynamic permission denial, host
  approval denial, pre-tool hook deny, loop-detector skip, and the
  final completion/rejection branch — and propagates the category to
  the ACP wire as `errorCategory`. Each early-failure path also emits
  a paired `ToolCall(Pending)` + `ToolCallUpdate(Failed)` so clients
  see a consistent two-event lifecycle for rejected calls instead of
  silence. The category is mirrored on the `tool_execution` transcript
  event metadata so replay engines see the same classification.
- **`tool_call_update.executor` tag (#691).** Distinguishes where a
  tool ran — `harn_builtin`, `host_bridge`, `{kind: "mcp_server",
  serverName: "..."}`, or `provider_native`. Lets ACP clients render
  "via X" badges, attribute latency by transport, and route errors
  correctly. Detection is automatic: the `_mcp_server` annotation that
  `mcp_list_tools` injects survives through bridge-proxied dispatch,
  so MCP-served tools tag correctly even when they physically call
  the host bridge. Provider-native server tools (OpenAI Responses
  `tool_search` etc.) emit a paired `tool_call`/`tool_call_update`
  alongside the existing `tool_search_*` events so badge-rendering
  clients don't have to special-case the search variants.
- **Harn-owned Ollama runtime settings (#676).** Centralizes Ollama
  `num_ctx` and `keep_alive` precedence, defaults, normalization, and
  warmup request shaping in `harn-vm`. Hosts can pass raw persisted
  preferences through `HARN_OLLAMA_NUM_CTX` and `HARN_OLLAMA_KEEP_ALIVE`
  without duplicating env precedence or keep-alive normalization.
- **Agents Protocol replay-as-API contract (#636).** Adds
  `POST /v1/tasks/{task_id}/replay` to the v1 OpenAPI surface with
  `exact`, `with_overrides`, and `from_checkpoint` modes, deterministic
  override maps, replay event metadata, and Receipt delta requirements.
  The new `agents-protocol-replay/` artifact documents the EventLog replay
  contract and ships fixtures for byte-identical replay Receipt conformance.
- **`unnecessary-cast` lint with autofix.** Flags conversion-builtin
  calls whose argument is already syntactically of the target type —
  `to_string("hi")`, `to_int(42)`, `to_float(1.5)`, `to_list([1,2,3])`,
  `to_dict({a: 1})`, and chained identity calls like
  `to_string(to_string(x))`. The autofix removes the redundant wrapper
  while preserving the inner expression's source formatting verbatim.
  Genuine conversions (`to_int("42")`, `to_float(5)`, `to_list(set([...]))`)
  do not trigger the lint.
- **`source.fixAll.harn` LSP code action.** The Harn LSP now advertises
  the `source.fixAll.harn` and `source.fixAll` code-action kinds and
  returns a single bulk action that applies every available autofix in
  the document at once. The bundled VS Code extension turns on
  `editor.formatOnSave` and `editor.codeActionsOnSave: { "source.fixAll.harn": "always" }`
  for `[harn]` files by default and contributes a
  `Harn: Apply All Autofixes` command palette entry that triggers the
  same bulk action on demand. Per-diagnostic quick-fixes (Cmd+.)
  continue to work unchanged.
- **HTTP server stdlib primitives (#650).** New in-process inbound HTTP
  server surface: `http_server`, `http_route`, `http_request_*`,
  `http_response_*`, before/after middleware, body-size limits,
  readiness/shutdown hooks, and synthetic `http_dispatch` for
  in-process integration tests. Routing supports path templates with
  typed params and ordered middleware. Conformance covers routing,
  param extraction, raw body, header shaping, status/header builders,
  body-limit rejection, readiness/shutdown, and middleware order.
- **HTTP server TLS configuration (#649).** Shared `harn-serve` TLS
  modes (plain HTTP, edge-terminated HTTPS, self-signed development
  HTTPS, PEM cert/key HTTPS) wired through `harn serve a2a` and
  `harn serve mcp --transport http` with HSTS response headers for
  edge and PEM modes. Adds matching HTTP stdlib helpers for TLS
  config and header policy.
- **Multipart form stdlib builtins (#651).** Buffered
  `multipart/form-data` parsing for inbound request bodies via
  `multipart_parse(body, content_type, options?)` with explicit
  `max_total_bytes` / `max_field_bytes` / `max_fields` limits, parsed
  field dicts (`name`, optional `filename` + `content_type`,
  normalized `headers`, raw `bytes`, UTF-8 `text` when valid),
  `multipart_field_bytes` / `multipart_field_text` accessors, and
  deterministic `multipart_form_data(fields, options?)` fixture
  generation.
- **Cookie and session stdlib helpers (#652).** Parse request `Cookie`
  headers into structured cookies (with ordered pairs, duplicate
  values, and invalid-segment reporting), serialize `Set-Cookie`
  values with `HttpOnly` / `Secure` / `SameSite` / `Path` / `Domain`
  / `Max-Age` / `Expires` / deletion support, sign and verify string
  cookie values and JSON stateless session tokens via HMAC-SHA256,
  and ship secure signed-session-cookie defaults plus a
  request/response cookie round-trip test helper.
- **WebSocket server stdlib primitives (#653).** New
  `websocket_server`, `websocket_route`, `websocket_accept`, and
  `websocket_server_close` builtins reuse the existing WebSocket
  send/receive/close-frame API for accepted inbound connections,
  including text/binary/ping/pong frames, close code+reason, max
  message limits, idle timeout, bearer upgrade auth, and bounded
  outbound backpressure. Wired through parser signatures, lint/type
  boundary awareness, LSP completions, IR/effect classification,
  generated highlighting, docs, and conformance.
- **Server-side SSE stdlib primitives (#655).** `text/event-stream`
  response handles, event formatting, writes, heartbeat/comment
  frames, flushing, close, cancel, disconnect observation, and
  deterministic mock-client reads — registered through the runtime
  stdlib, parser signatures, LSP constants, lint/type boundary
  handling, IR side-effect classification, and autonomy mutation
  policy.
- **Signed URL stdlib helpers (#656).**
  `signed_url(base, claims, secret, expires_at, options?)` for
  absolute URLs and absolute paths plus
  `verify_signed_url(url, secret_or_keys, now, options?)` with
  constant-time signature comparison, expiry/skew handling, URL-safe
  HMAC-SHA256 signatures, and optional `kid` key rotation.
  Conformance covers canonicalization, tampering, expiry/skew, key
  rotation, and path signing.
- **Postgres stdlib builtins (#654).** VM-native Postgres surface:
  `pg_pool`, `pg_connect`, `pg_close`, `pg_query`, `pg_query_one`,
  `pg_execute`, `pg_transaction`, `pg_mock_pool`, `pg_mock_calls`.
  Supports URL, env, and secret-backed connection sources; pool
  timeout / TLS / application name / statement cache options;
  parameterized queries; transaction-local RLS settings via
  `set_config`; and decoding for JSON/JSONB, UUID, date/time/
  timestamp/timestamptz, bytea, numbers, booleans, strings, and
  nulls. Live coverage runs when `HARN_TEST_POSTGRES_URL` is set;
  mock fixtures and call capture stay deterministic in default CI/dev
  runs.
- **JSON pointer + jq query stdlib (#624).** RFC 6901 `json_pointer`,
  `json_pointer_set`, `json_pointer_delete` with proper escaping and
  copy-on-write updates. New `jq` and `jq_first` builtins evaluate
  the accepted v1 jq subset locally. Documentation and conformance
  cover pointer mutation/escaping plus jq operator coverage.
- **AST host builtins (#621).** Lights up
  `hostlib_ast_parse_file`, `hostlib_ast_symbols`, and
  `hostlib_ast_outline` for 22 host languages (TypeScript, TSX,
  JavaScript, JSX, Python, Go, Rust, Java, C, C++, C#, Ruby, Kotlin,
  PHP, Scala, Bash, Swift, Zig, Elixir, Lua, Haskell, R) on top of
  pinned `tree-sitter` 0.26 grammars. Per-language extractors share
  `walk_named` + `named_decl_with_keyword` / `push_func` helpers;
  `symbols`/`outline` carry signatures with 0-based row/col
  coordinates; `parse_file` flattens the tree breadth-first.
- **Compression stdlib builtins (#613).** Added in-memory
  `gzip_encode`/`gzip_decode`, `zstd_encode`/`zstd_decode`,
  `brotli_encode`/`brotli_decode`, `tar_create`/`tar_extract`, and
  `zip_create`/`zip_extract` builtins. Encoders accept strings or
  bytes, decoders return bytes, tar extraction preserves entry modes,
  and conformance now covers all supported formats.
- **Timezone-aware datetime stdlib (#614).** `date_parse` now uses
  chrono-backed RFC 3339 / ISO 8601 parsing before the legacy
  digit-extraction fallback, `date_format` supports full strftime
  formatting and pre-epoch timestamps, and `date_now()` includes an
  additive `iso8601` field. Added `date_now_iso`,
  `date_in_zone`, `date_to_zone`, `date_from_components`,
  `date_add`, `date_diff`, `weekday_name`, `month_name`, and
  duration helpers (`duration_ms`, `duration_seconds`,
  `duration_minutes`, `duration_hours`, `duration_days`,
  `duration_to_seconds`, `duration_to_human`) with IANA timezone
  support via `chrono-tz`. Migration note: malformed inputs that
  relied on `date_parse` digit extraction still fall back, but
  impossible calendar dates now throw instead of rolling through
  timestamp arithmetic.

- **`harn-hostlib` process-lifecycle tools (#568, #606).** Implemented
  `run_command`, `run_test`, `run_build_command`,
  `inspect_test_results`, and `manage_packages` under the gated
  `tools:deterministic` hostlib surface. Process spawns use argv-only
  execution, cwd/env/stdin/timeout handling, structured build diagnostic
  parsing, process-local test result handles, package-manager command
  assembly, and the public `harn_vm::process_sandbox` helpers so active
  Linux seccomp/landlock and macOS sandbox-exec policies still apply.
- **Stdlib scripting helpers (#618).** Added reproducible RNG handles
  via `rng_seed(...)` and seeded overloads for `random`,
  `random_int`, `random_choice`, and `random_shuffle`; promoted
  `mean` / `median` / `variance` / `stddev` / `percentile` and
  collection helpers (`chunk`, `window`, `group_by`, `partition`,
  `dedup_by`, `flat_map`) to global builtins; added `uuid_parse`,
  `uuid_v5`, `uuid_v7`, and `uuid_nil`; shipped
  `unicode_normalize`, `unicode_graphemes`, and `str_pad`; added
  `sync_rwlock_acquire` and `channel_select`; and extended regex
  support with optional match flags plus `regex_split`. Each area
  now has dedicated conformance coverage.

- **Scanner host builtins (#566).** `harn-hostlib`'s `scanner/` module
  gains live implementations of `scan_project` and `scan_incremental`.
  Ports the deterministic intake pipeline from
  `Sources/BurinCore/Scanner/CoreRepoScanner.swift` —
  `.gitignore`-aware file discovery (git ls-files when available, falling
  back to `ignore`/`walkdir`), regex-based symbol extraction (Swift,
  Shell, Dart, and the generic fallback faithfully ported from
  `SymbolExtractor.swift`), import parsing for 13 languages
  (`ImportParser.swift`), reference-count + churn + importance scoring,
  source ↔ test pairing using burin-code's per-language test patterns,
  folder aggregates + project metadata (language stats, detected test
  command, code-pattern hints), sub-project boundary detection
  (`SubProjectDetector.swift`), and a token-budgeted text repo map
  (`RepoMapBuilder.swift`). Output shape mirrors burin-code's `ScanResult`
  exactly so bridge consumers can use the Rust pipeline without changing
  their result parser. `scan_project` persists a snapshot to
  `<root>/.harn/hostlib/scanner-snapshot.json`; `scan_incremental` diffs
  the workspace against that snapshot (mtime-based by default,
  optionally driven by an explicit `changed_paths` list) and falls back
  to a full rescan when the diff exceeds ~30% of the workspace or the
  snapshot is missing. Unlike the deterministic-tools surface the
  scanner is ungated — emitting a `ScanResult` is read-only and the
  snapshot lives in the managed `.harn/` directory.

- **General-purpose scripting support.** Harn scripts can now start
  with a `#!/usr/bin/env harn` shebang, and the formatter preserves
  that line on round-trip. Tree-sitter highlights the shebang as a
  comment while ordinary `#` tokens elsewhere remain invalid.

- **stderr / stdin / TTY builtins**:
  - `eprint(s)`, `eprintln(s)` — write to stderr (separate from stdout
    capture).
  - `read_stdin()` — slurp piped stdin to a string; `read_line()` —
    line-by-line iterator-style read; both return `nil` at EOF.
  - `is_stdin_tty()` / `is_stdout_tty()` / `is_stderr_tty()` — uses
    `std::io::IsTerminal` so `harn` programs can adapt to pipelines.
  - `set_color_mode("auto"|"always"|"never")` — controls ANSI emission
    from `color`/`bold`/`dim`. Auto honors `NO_COLOR` and `FORCE_COLOR`
    and only emits when stdout is a TTY (the previous behavior was to
    always emit, which produced garbage in pipes and on Windows
    consoles without VT100).

- **Mockable clock + sleep**:
  - `now_ms()` — wall-clock millis since epoch.
  - `monotonic_ms()` — monotonic millis (unaffected by NTP jumps).
  - `sleep_ms(n)` — async sleep; under a clock mock, advances mocked
    time instantly instead of suspending the runtime.
  - `mock_time(ms)` / `advance_time(ms)` / `unmock_time()` — let
    Harn-level tests pin time deterministically. `timestamp` and
    `elapsed` now route through this clock so existing builtins are
    mockable too.

- **stdin / TTY mocks for tests**: `mock_stdin(text)` /
  `unmock_stdin()`, `mock_tty(stream, bool)` / `unmock_tty()`,
  `capture_stderr_start()` / `capture_stderr_take()` — all from `.harn`
  test code.

- **Exit code from `main()` return value**:
  - `return n: int`           → process exits with `n` (clamped 0..=255).
  - `return Err(msg)`         → writes `msg` to stderr, exits 1.
  - `return Ok(_)` / implicit → exits 0.
  - The `exit(code)` builtin still works for early termination.

- **Filesystem helpers**:
  `glob(pattern, base?)`, `walk_dir(root, opts?)`,
  `move_file(src, dst)`, `read_lines(path)`. Backed by `globset` /
  `walkdir`.

- **CSV** (new `stdlib/csv.rs`): `csv_parse(text, opts?)` and
  `csv_stringify(rows, opts?)`. Supports `headers: bool` (returns
  list-of-dicts when on, list-of-lists otherwise) and
  `delimiter: ","`.

- **URL parsing & building** (new `stdlib/url_parse.rs`):
  `url_parse(s)` returns `{scheme, host, port, path, query, fragment,
  username, password}`; `url_build(parts)` round-trips back.
  `query_parse(s)` returns a list of `{key, value}` (preserves
  duplicate keys, RFC 3986 percent-decoded); `query_stringify(pairs)`
  builds query strings with `+`/`%`-encoding.

- **Modern crypto** (`stdlib/crypto.rs`):
  - `sha3_256(input)`, `sha3_512(input)`, `blake3(input)`.
  - `ed25519_keypair()`, `ed25519_sign(priv_hex, msg)`,
    `ed25519_verify(pub_hex, msg, sig_hex)` for signatures.
  - `x25519_keypair()`, `x25519_agree(priv_hex, peer_pub_hex)` for
    Diffie-Hellman key agreement.
  - `jwt_verify(alg, token, key)` for HS256/RS256/ES256 — completes
    the existing `jwt_sign` round-trip.
- **`harn persona pause/resume/disable --at <RFC3339>` (#611).**
  Mirrors the existing `--at` flag on `persona status / tick / trigger
  / spend` so all wall-clock-sensitive persona commands share a single
  override surface. Useful for deterministic replay and for fixing a
  pre-existing UTC-day-boundary flake in
  `persona_runtime_status_tick_and_budget_are_persisted`.
- **HTTP client power features for stdlib builtins (#616).** Added
  `http_download` for file-backed transfers plus
  `http_stream_open` / `http_stream_read` / `http_stream_info` /
  `http_stream_close` for pull-based response streaming. HTTP requests
  now also support multipart form uploads, proxy routing with optional
  basic auth and bypass lists, per-phase timeout controls
  (`total_timeout_ms`, `connect_timeout_ms`, `read_timeout_ms`),
  custom trust material / client identities via `tls`, certificate pin
  verification with `pinned_sha256`, and explicit decompression control.
  Conformance and VM coverage now exercise multipart bodies, streamed
  reads, file downloads, proxy forwarding, and pinned/custom-TLS flows.

### Changed

- **Release scripts: harden new-workspace-crate first-release path
  (#609).** When a "Prepare vX.Y.Z release" PR adds a new workspace
  crate that an already-published crate (e.g. `harn-cli`) depends on,
  cargo's dependency-resolution step inside `cargo package -p harn-cli`
  fails with `no matching package named <new-crate> found` — even with
  `--no-verify`, which only skips the staged build. The Bump Release
  workflow's audit lane therefore fails for the first release that
  ships such a crate. Bootstrap pattern, in priority order:
  - **Recommended:** before landing the prepare PR, manually
    `cargo publish -p <new-crate> --no-verify --allow-dirty` from
    main HEAD to seed the crate at the current workspace version.
    Subsequent releases proceed through the normal automated flow.
  - **Recovery:** if the prepare PR already landed and the bump
    workflow is failing, manually re-trigger Bump Release (or
    Finalize Release) with `bootstrap_new_crates: true`. The flag
    sets `HARN_BOOTSTRAP_NEW_CRATES=1` for `release_ship.sh`, which
    skips the publish dry-run and tells `verify_crate_packages.sh`
    to skip the `harn-cli` package check. The real publish later
    uses `cargo publish --workspace`, which orders intra-workspace
    deps correctly. `scripts/publish.sh`'s `WORKSPACE_CRATES`
    fallback list now includes `harn-hostlib` between `harn-lsp`
    and `harn-cli` so the per-crate fallback covers it. The
    merge-captain runbook (`.claude/commands/release-harn.md`) and
    the burin-code merge-captain skill carry the same pre-flight.

### Fixed

- **Cross-platform `process.exec` host capability**:
  `crates/harn-vm/src/stdlib/host.rs` previously hardcoded
  `/bin/sh -lc` for the `process.exec` host operation, breaking on
  Windows. Now dispatches to `cmd /C` on Windows, `/bin/sh -lc`
  elsewhere — mirroring the existing `process.shell` builtin.

- **`color()` / `bold()` / `dim()` on non-TTY**: These previously
  emitted raw ANSI escapes unconditionally, polluting piped output and
  rendering as garbage on legacy Windows consoles. They now honor
  `set_color_mode` and `NO_COLOR`/`FORCE_COLOR` env vars and the
  computed TTY state of stdout.

- **`harn connector check` registers `store_*` builtins** on the connector's
  base VM, matching the runtime that backs `harn run` /
  `harn orchestrator serve`. Previously connectors that used
  `store_get`/`store_set`/`store_delete` for persistent state (e.g. for
  installation-token caches) failed during contract verification with
  `Undefined builtin: store_*`.

### Spec

- **Agents Protocol v1 narrative spec (#646).** Adds the authoritative
  spec for Harn Agents Protocol v1 at `spec/AGENTS_PROTOCOL.md` plus
  an mdBook include at `docs/src/spec/agents-protocol/v1.md`. Covers
  the resource model (Persona, Workspace, Session, Task, Branch,
  Message/Part/Artifact, AgentCard, Event, Receipt, Memory, Vault,
  Connector, Skill, Outcome, Quota), REST/SSE/WebSocket transports,
  API key + OAuth2 client credentials auth, `Idempotency-Key`
  semantics, A2A-aligned task lifecycle, event/error taxonomies, and
  core/extended/receipts/replay conformance levels.
- **Agents Protocol stdlib gap audit (#648).** Adds
  `spec/agents-protocol-stdlib-audit.md` — first-cut survey of stdlib
  gaps blocking a Harn-native Harness reference implementation, with
  cross-references to the implementation sub-tickets that ship in
  this release (#649–#656).

### Platform

- **Windows process sandbox (#626).** New Windows process launcher
  runs policy-scoped commands in a no-capability `AppContainer` and
  restrictive `Job Object`, granting AppContainer ACL access only to
  workspace roots and cleaning those grants up after the child exits.
  Internal exec/shell/workflow verify command paths now route through
  a shared `command_output` helper. Brings macOS sandbox-exec / Linux
  seccomp+landlock parity to Windows for `process_sandbox` consumers.

### Tests

- **Stabilize orchestrator/connector subprocess tests (#657).** Five
  tests that intermittently timed out at the 60s nextest ceiling
  (`slack_url_verification_returns_plaintext_challenge`,
  `slack_webhook_acknowledges_before_handler_finishes`,
  `stream_trigger_route_uses_generic_stream_connector`,
  `watch_mode_reloads_manifest_changes`,
  `restart_after_emit_does_not_duplicate_cron_dispatch`) now run in
  1.0–6s. Fixes were three independent harness flakes: tightened
  `PROCESS_FAIL_FAST_TIMEOUT` budgets too aggressive for cold macOS
  dyld+amfi lookups, busy-poll file waits replaced with `notify`-based
  watches, and generous spawn deadlines for cold-start orchestrator
  binaries.

### CI

- **Aggregate CI status gate (#625).** Added a final `Check status`
  job that always evaluates the required CI jobs, with docs deployment
  routed through this aggregate gate. Simplifies branch-protection
  configuration to a single required check.
- **Windows CI smoke job** (`.github/workflows/ci.yml`). Builds the
  workspace and runs `harn-lexer` / `harn-parser` / `harn-vm` /
  `harn-fmt` / `harn-lint` / `harn-modules` unit tests on
  `windows-latest`, plus a `harn run` smoke. Existing
  Unix-gated tests (`#![cfg(unix)]` on the orchestrator suite,
  `cfg(target_os = ...)` on sandbox tests) auto-skip.
- **Windows job is now path-conditional and faster.** PRs that don't
  touch `crates/`, `conformance/`, `Cargo.toml`, `Cargo.lock`, `.cargo/`,
  `rust-toolchain.toml`, or the CI workflow itself skip the Windows
  build entirely (a no-op alias job satisfies branch protection).
  When the job does run it now uses `cargo check --workspace --tests`
  for compile sanity plus a focused `cargo build --bin harn`, instead
  of `cargo build --workspace --tests --bin harn` — roughly halving
  Windows wall time on cache misses. `merge_group` and `push` events
  always run the full Windows job.
- **Build warnings are errors workspace-wide.** CI runs with
  `RUSTFLAGS=-D warnings` so platform-specific build warnings
  (Windows-only deprecations, dead_code under `cfg`-gates, etc.) can't
  silently accumulate. Clippy already ran with `-D warnings`; this
  closes the same gap for `cargo build` / `cargo check`.
- **Windows release artifact**:
  `.github/workflows/release.yml` matrix gains
  `x86_64-pc-windows-msvc` and packages a `harn-...zip` alongside the
  Linux/macOS tarballs.

## v0.7.39

### Added

- **Atom primitive for Harn Flow (#601).** New
  `crates/harn-vm/src/flow/atom.rs` foundational primitive for Harn
  Flow (parent epic #571): content-addressed, signed, and
  constructively invertible.
  `Atom { id, ops, parents, provenance, signature, inverse_of }`
  carries a `Provenance { principal, persona, agent_run_id,
  tool_call_id, trace_id, transcript_ref, timestamp }` plus dual
  Ed25519 signatures (principal + persona) over the `AtomId`, ready
  to chain into the trust graph in a follow-up.
  `TextOp::{Insert, Delete}` apply / invert with deletes carrying the
  removed bytes so the inverse is reconstructible without consulting
  the document. Two round-tripping encodings on the same struct:
  serde-JSON for interchange / event-log payloads and a versioned
  length-prefixed canonical binary form (deterministic, used for
  hashing and storage) — both decoders re-derive and verify the
  content hash.

- **`harn-hostlib` crate scaffold (#563).** New opt-in crate housing
  code-intelligence and deterministic-tool host builtins ported from
  `burin-code`'s Swift `BurinCore` (tree-sitter AST, trigram/word index,
  repo scanner, filesystem watcher, search/file/git/process tooling).
  Every method registered today returns `HostlibError::Unimplemented`;
  follow-up issues fill in module bodies. Module skeletons + JSON
  Schema 2020-12 contracts ship in this PR so `burin-code`'s
  schema-drift tests can lock the public surface immediately. Wired
  into `harn-cli`'s ACP server behind the default-on `hostlib` cargo
  feature.
- **Deterministic tool host builtins (#567).** `harn-hostlib`'s
  `tools/` module gains live implementations for `search` (ripgrep
  semantics via `grep-searcher` + `ignore` with structured matches and
  context lines), `read_file` (utf-8 + base64 with offset/limit and
  truncation reporting), `write_file` (parent-dir creation, overwrite
  guard, base64 input), `delete_file` (recursive opt-in for
  directories), `list_directory` (sorted entries, hidden filter,
  pagination), `get_file_outline` (language-agnostic regex extractor
  matching `ast.outline` shape), and `git` (read-only inspection:
  `status`, `diff`, `log`, `blame`, `show`, `branch_list`,
  `current_branch`, `remote_list` — shelling out to system `git` with
  arg-list invocations only, never `sh -c`, plus rev-string validation
  that rejects flag lookalikes and control bytes). The surface is
  gated by a per-session opt-in: pipelines call
  `hostlib_enable("tools:deterministic")` before any of these seven
  deterministic tools will execute, otherwise calls fail with a
  structured error pointing at the enable builtin.
- **Code-index host builtins (#565).** `harn-hostlib`'s `code_index/`
  module now ships a working trigram + word index, dep graph, file
  table, and import resolver — ports the Swift `BurinCodeIndex` actor
  into pure Rust. Five builtins go live behind the schemas locked in
  by #563: `hostlib_code_index_query` (trigram-accelerated literal
  substring search with case-insensitive default, `scope` path filter,
  and `max_results` truncation), `hostlib_code_index_rebuild` (depth-
  first walk honouring the same skip-dirs / sensitive-file filter
  that `BurinCodeIndex/FilteredWalker.swift` enforced — node_modules,
  `.git`, build artefacts, anything matching the credentials shape are
  pruned before descent), `hostlib_code_index_stats` (file count,
  distinct trigrams, distinct words, byte estimate, last rebuild
  timestamp), `hostlib_code_index_imports_for` (per-file imports list
  with `module` / `resolved_path` / `kind` triples), and
  `hostlib_code_index_importers_of` (reverse import lookup). Import
  resolution is data-driven via
  `data/code_index_import_rules.json` (Python, TS/JS, Java/Kotlin,
  Scala, C#, PHP, Elixir, Haskell, Lua, Ruby, C/C++, Zig, R, Swift,
  Rust, Go) — adding a language is a JSON edit. The trigram packing,
  word-index tokenisation, and FNV-1a content hashing match Swift
  byte-for-byte so snapshots could in principle round-trip. Five
  builtins are now exposed via `install_default`; embedders that want
  isolated workspaces construct independent `CodeIndexCapability`
  instances.

- **Fair-share scheduler for worker-queue claims (#477).** New
  deficit-round-robin policy in front of `WorkerQueue::claim_next` so a
  hot tenant, binding, or trigger id can no longer monopolise a shared
  queue. Default remains FIFO — single-tenant deployments see no
  behaviour change unless they opt in via `HARN_SCHEDULER_STRATEGY=drr`.
  Configurable via `HARN_SCHEDULER_*` env vars: fairness key
  (`tenant`, `binding`, `trigger-id`, `tenant-and-binding`), per-key
  weights, starvation-age promotion threshold, and per-key concurrency
  caps. Existing per-binding flow-control gates still apply *after*
  selection. `harn orchestrator queue ls --json` now exposes a
  `scheduler` block with per-fairness-key deficit, weight, in-flight,
  selected/deferred totals, and oldest-eligible age. New Prometheus
  metrics: `harn_scheduler_selections_total`,
  `harn_scheduler_deferrals_total`,
  `harn_scheduler_starvation_promotions_total`,
  `harn_scheduler_deficit`,
  `harn_scheduler_oldest_eligible_age_seconds`. See
  `docs/src/orchestrator/worker-dispatch.md` for the full reference.

### Deprecated

- **Rust-side GitHub, Slack, Linear, and Notion provider connectors
  (#602, #446).** New deployments should configure the corresponding
  pure-Harn connector packages
  ([harn-github-connector](https://github.com/burin-labs/harn-github-connector),
  [harn-slack-connector](https://github.com/burin-labs/harn-slack-connector),
  [harn-linear-connector](https://github.com/burin-labs/harn-linear-connector),
  [harn-notion-connector](https://github.com/burin-labs/harn-notion-connector))
  by pointing `[[providers]]` at `connector = { harn = "..." }`.
  `harn orchestrator serve` now emits a single `warning:` line per
  affected provider at startup when the Rust default is auto-selected; the
  warning is silenced once a Harn package override is in place. Cron, the
  generic webhook connector with HMAC verification, A2A push, stream
  ingress, raw-body access, and signing primitives stay in core. Adds a
  `Rust connectors → Harn packages` migration guide under `docs/migrations/`
  and deprecation banners on each affected connector reference page.
  `harn connector check` fixtures can also assert dedupe keys, signature
  state, provider-payload subsets, and immediate-response status/body so
  first-party pure-Harn connector repos can pin Rust payload-shape parity
  in CI. No Rust connector business logic has been removed; the timeline
  for that removal is gated on the prerequisites called out on issue #446.

### Removed

- **`harn mcp-serve` (#594).** The hidden legacy alias for serving a
  `.harn` tool bundle as an MCP server is gone. Use `harn serve mcp
  <file>` instead — it auto-detects whether the script exposes its
  surface through `pub fn` exports (the recommended path) or through
  the `mcp_tools(...)` / `mcp_resource(...)` / `mcp_prompt(...)`
  registration builtins, and serves the appropriate one over stdio.
  `--card <PATH_OR_JSON>` carried over to `harn serve mcp` and is
  honored for the script-driven surface. Update any
  `claude_desktop_config.json` / Cursor / Continue launch snippets that
  pass `["mcp-serve", "<file>"]` to `["serve", "mcp", "<file>"]`.

## v0.7.38

### Added

- **`harn persona status --at <RFC3339>` (#592).** Mirrors the
  existing `tick --at` flag: pins the budget-window query to a
  deterministic UTC moment instead of using the wall clock. Lets
  tests pair a `tick --at <T>` with a `status --at <T>` and assert
  on `spent_today_usd` / `tokens_today` without flaking when the
  test happens to run after `<T>`'s UTC midnight.

- **Optional subscript `obj?[index]` (#596).** Symmetric counterpart to
  `obj?.member`. Returns `nil` when the receiver is `nil`; otherwise
  indexes normally. Lets connector authors safely chain into lists or
  dicts that may be missing — `payload?.commits?[0]?.timestamp` now
  parses and short-circuits hop-by-hop. Previously the parser tried
  to interpret `?[` as the start of a ternary and bailed. Adds the
  `OptionalSubscriptAccess` AST node, `SUBSCRIPT_OPT` opcode, and
  formatter/lint/IR/LSP/viz/preflight handling alongside the existing
  `SubscriptAccess` paths.

### Changed

- **`http_mock` re-registration now replaces (#593).** Calling
  `http_mock(method, url_pattern, ...)` a second time with the same
  `(method, url_pattern)` tuple now replaces the prior mock instead of
  appending behind it. Previously the first registration matched
  forever and the second was dead code, which made it surprisingly
  hard to override a per-case response (e.g. a happy `200` followed by
  a deliberate `429` for a rate-limit cap test) without first calling
  `http_mock_clear()`. Distinct `(method, url_pattern)` tuples are
  unaffected.

### Fixed

- **`persona_runtime_status_tick_and_budget_are_persisted` UTC-day
  flake (#592).** The test pinned `tick --at 2026-04-24T12:30:00Z`
  but read `status` against the wall clock, so the
  `spent_today_usd == 0.25` assertion silently dropped to `0.0`
  every time the test ran after the tick's UTC midnight (i.e.
  basically any time of day in PT/CT/ET). The status command now
  accepts the same `--at` flag and the test threads it through.

- **Trailing binary operator + newline now parses (#595).** A binary
  operator at the *end* of a line followed by the right operand on the
  next line (e.g. `let x = a ??\n  b`) previously errored with
  `expected expression, found \n`. Only the *leading*-operator
  continuation form (`let x = a\n  ?? b`) worked. Both forms now
  parse identically. Affects `|>`, `??`, `||`, `&&`, `==`, `!=`,
  `<`, `>`, `<=`, `>=`, `in`, `not in`, `+`, `-`, `*`, `/`, `%`, `**`.

## v0.7.37

### Added

- **Eval pack manifest v1 (#450).** Adds eval-pack v1 manifest structs
  and TOML/JSON loading to `harn-vm`, evaluates portable packs through
  existing replay fixtures, baseline diffs, deterministic/HITL
  assertions, and cost/latency/token/stage thresholds, and surfaces
  them via `harn eval harn.eval.toml` and
  `harn test package --evals` with `[package].evals` discovery.
  Documented fixture/rubric kinds, judge metadata, and threshold
  severities.
- **GitLab connector listed in the connector catalog (#588).**
  `docs/src/connectors/catalog.md` now registers the pure-Harn
  [`burin-labs/harn-gitlab-connector`](https://github.com/burin-labs/harn-gitlab-connector)
  package with its auth quirks, supported trigger event types, and
  outbound surfaces.
- **Continuous persona runtime primitives (#462).** Adds an
  event-sourced `persona.runtime.events` runtime with lifecycle state,
  single-writer leases, schedule and external trigger wake receipts,
  pause/resume/disable controls, per-persona budget enforcement, and
  stable `harn persona status <name> --json` output for hosts.
- **Connector catalog and trigger example library (#177).** Added a
  connector catalog, generated `docs/llm/harn-triggers-quickref.md`
  from the live trigger provider catalog, and expanded
  `examples/triggers/` into a ready-to-customize library with
  `README.md` and `SKILL.md` metadata per recipe.
- **Multi-tenant orchestrator isolation (#190).** Adds a persisted
  tenant registry under `harn orchestrator tenant` with per-tenant
  namespaced event log, `TenantScope` / `TenantEventLog` /
  `TenantSecretProvider` primitives, API-key hashing and resolution,
  per-tenant budgets, and topic/namespace helpers. Dispatcher routing
  now scopes all dispatch traffic to tenant topics so one tenant's
  load cannot leak into another's queues.
- **Package authoring workflow (#471).** Extends `harn.toml`
  `[package]` with `description`, `license`, `repository`, `harn`
  (version range), and `docs_url`, adds `harn package new <name>`,
  `harn package validate`, and `harn package publish --dry-run`, and
  documents the authoring flow in `docs/src/package-authoring.md`.
- **OpenTrustGraph v0 spec artifact (#449).** Publishes
  `opentrustgraph-spec/` as the canonical v0 artifact inside the Harn
  repo with a chain-export JSON Schema, approval-evidence rules,
  valid tier-transition fixture, and an invalid missing-approval
  fixture. Fixtures are deterministic chain envelopes and are
  validated from Harn runtime tests; the artifact is cross-linked
  from docs, portal docs, and the README for Harn Cloud and
  supervision references.
- **Friction context-pack primitives (#452).** Introduces structured
  friction event primitives with privacy-focused normalization, a
  JSONL sink, and in-memory event inspection; adds context-pack
  manifest validation and deterministic suggestion generation from
  repeated friction evidence; extends eval packs with
  `friction-events` fixtures and `context-pack-suggestion` assertions.
- **Per-agent dynamic permissions (#529).** `agent_loop`,
  `sub_agent_run`, workflow stages, and `spawn_agent` now accept a
  `permissions.allow` / `permissions.deny` dict with tool-name globs,
  argument pattern lists, keyed argument patterns, and VM predicates
  over tool args. Child agents inherit parent scopes by intersection,
  so delegation cannot widen trust. Permission denials surface as
  structured tool results rather than opaque failures.
- **Portal DLQ management surface (#192).** Adds `/dlq` to the React
  portal with filterable DLQ list, error-class groups, active alert
  summary, detail inspector, replay, drift-accept replay, purge,
  export-fixture, and bulk controls. Admin endpoints expose DLQ
  list/detail/replay/purge/export plus bulk replay and purge, reading
  from `trigger.dlq` and normalizing both dispatcher `dlq_moved` and
  stdlib `dlq_entry` shapes. DLQ records are tagged with a derived
  `error_class` at move/upsert time, and per-trigger DLQ alert
  destinations/thresholds configured in `harn.toml` surface through
  the portal API.
- **Supervisor trees and restart policies (#484).** Adds supervisor
  lifecycle builtins for named child-task supervision with
  state/events/metrics introspection, runtime debug exposure, and
  cooperative stop/drain. Restart policies support
  `never`/`on_failure`/`always` modes, max restart windows,
  exponential backoff, deterministic jitter, circuit-open probing,
  and `one-for-one`/`one-for-all`/`rest-for-one`/`escalate`
  strategies. Documented in the spec, concurrency guide, and
  builtins reference.

### Changed

- **Enforced stdlib mirror parity in CI (#552).** The Format check job
  now asserts every `crates/harn-vm/src/stdlib*.harn` file matches the
  corresponding `crates/harn-modules/src/stdlib/` mirror byte-for-byte,
  preventing drift between the VM's embedded stdlib and the packaged
  module surface.
- **Fixed stranded-envelope conformance flake (#553).** The orchestrator
  recovery test now gates on a `/readyz` poll after the listener URL
  is known, eliminating a race where envelopes could be flushed before
  the server was ready under CI load.
- **Stabilized connector tests and dispatcher timing (#560).** Connector
  test suites (GitHub, Slack, Linear, Notion) now share a single HTTP
  stub helper in `connectors::test_util`, eliminating ad-hoc local
  mocks. Dispatcher timing tests moved to Tokio paused time so timing
  assertions no longer depend on wall-clock scheduling — reduces flake
  risk under CI load.
- **Unified `agent_loop` `llm_retries` default at 4 (#554).** Previously
  the bridge-aware registration defaulted to 4 while the non-bridge
  registration and `sub_agent_run` defaulted to 3. All three paths now
  share a single `DEFAULT_AGENT_LOOP_LLM_RETRIES` constant, so an
  unqualified `agent_loop` or `sub_agent_run` call retries transient
  provider errors up to four times (five attempts total) regardless of
  entry point. Callers that explicitly pass `llm_retries` are
  unaffected.
- **ACP server moved into `harn-serve::adapters::acp` (#557).** The stdio
  ACP adapter is now packaged in the `harn-serve` crate and fronted by
  `harn serve acp <file.harn>`. **Breaking:** the top-level `harn acp`
  command is removed with no compatibility alias. Editor and IDE hosts
  must invoke `harn serve acp <file.harn>` directly.

## v0.7.36

### Added

- **`assemble_context` builtin (#530).** Adaptive context-assembly
  primitive that returns a packed prompt of relevance-ranked snippets
  bounded by a token budget. Pipelines can pass typed source records
  and per-source metadata; the builtin handles ranking, dedup, and
  truncation so workflows don't reimplement the same context-packing
  logic.

### Changed

- **`llm_call` throws a categorized error dict (#534).** The value
  caught in `catch (e)` from a failed `llm_call` is now
  `{category, message, retry_after_ms?, provider, model}` — the same
  shape `llm_call_safe` exposes under `r.error`. Scripts can dispatch
  on `e.category` against the 13 canonical `ErrorCategory` strings
  (`"rate_limit"`, `"timeout"`, `"overloaded"`, `"server_error"`,
  `"transient_network"`, `"schema_validation"`, `"auth"`,
  `"not_found"`, `"circuit_open"`, `"tool_error"`, `"tool_rejected"`,
  `"cancelled"`, `"generic"`) and honor `e.retry_after_ms` instead of
  parsing the error message. **Breaking:** callers that
  string-matched the previous thrown message (`e.contains("429")`)
  must switch to `e.category == "rate_limit"` or use `e.message` to
  keep the substring check. The `error_category(e)` and
  `is_rate_limited(e)` helpers accept either the new dict shape or a
  legacy string — no change for callers that already use them.
  `llm_mock({error: {...}})` gained an optional
  `retry_after_ms: <int>` field for tests that exercise the
  rate-limit path end-to-end.

## v0.7.35

### Added

- **Ergonomic `llm_call_structured` helper (#531).** Adds
  `llm_call_structured(prompt, schema, options?)` and
  `llm_call_structured_safe(prompt, schema, options?)` to the stdlib
  (non-bridge and ACP bridge paths). Schema is the second positional
  argument, the schema-validated-JSON defaults
  (`response_format: "json"`, `output_validation: "error"`,
  `schema_retries: 3`) are forced unless the caller overrides them,
  and the helper returns the validated `.data` payload directly.
  `Schema<T>` in the second argument position narrows the return type
  to `T`. The `*_safe` variant returns the `{ok, data, error}`
  envelope mirroring `llm_call_safe`.
- **Formatter regression tests + conformance fixture for multi-`??`
  chains.** `harn fmt` already wraps null-coalescing chains with each
  operator at line start and a +2-space continuation indent, but the
  invariant was only tested for two operands; added unit tests for
  `n ≥ 3`-operand chains and method-chain-plus-`??` shapes, plus a
  conformance fixture under `conformance/tests/fmt/` that locks in
  both formatter stability and runtime right-to-left fallback
  semantics.
- **Cancellation contract documented.** `docs/llm/harn-quickref.md`
  now has a dedicated "Cancellation" section covering Ctrl-C /
  `cancel(task)` / ACP `session/cancel` semantics across `llm_call`,
  mid-tool-call, and between-turn `agent_loop` states.

### Changed

- **BREAKING: Namespaced `agent_loop` result shape (#532).** The flat
  `iterations`, `duration_ms`, `tools_used`, `successful_tools`,
  `rejected_tools`, and `tool_calling_mode` keys are gone. Metrics now
  live under `result.llm.{iterations, duration_ms, input_tokens,
  output_tokens}` and tool invocation data under `result.tools.{calls,
  successful, rejected, mode}`. Top-level keys (`status`, `text`,
  `visible_text`, `transcript`, `task_ledger`, `trace`, `daemon_state`,
  `daemon_snapshot_path`, `deferred_user_messages`,
  `ledger_done_rejections`) are unchanged. Callers should migrate to
  the nested paths — there is no `result_shape` flag or legacy
  fallback. Internal planner-round summarization in run records reads
  the new paths; docs, quickref, and conformance fixtures are updated.
- **Host-agnostic defaults and comments.** Scoped `harn new`'s default
  system prompt, the default conversation / archived-message
  compaction prompts, the Cargo package description, and scattered
  doc-comment + docs references away from "coding agent" / "Burin" /
  "IDE" wording. Legitimate integration points (DAP custom
  `burin/promptProvenance`, bridge module, docs about IDE-hosts)
  remain as-is.
- **Ollama env-var rename.** `HARN_OLLAMA_NUM_CTX` and
  `HARN_OLLAMA_KEEP_ALIVE` are now the canonical host-agnostic
  overrides; the previous `BURIN_OLLAMA_*` names are dropped
  (breaking for anyone still setting them — switch to the `HARN_`
  prefix). `HARN_ACP_TRACE_CALLS` similarly replaces
  `BURIN_TRACE_HARN_CALLS`.
- **Unified `llm_call` retry default.** The bridge-aware `llm_call`
  path now defaults `llm_retries` to 2, matching the non-bridge path
  and the documented "transient errors retry, schema errors don't"
  posture. Pass `llm_retries: 0` to opt out.
- **`llm_call` schema-retry is now a single-turn correction (#533).**
  The invalid assistant response is no longer replayed across retries.
  Each retry replays the caller's original messages plus one appended
  corrective user turn — avoiding the `user → assistant(bad) →
  user(nudge) → assistant` shape that confuses smaller / local models.
  The `SchemaRetry` trace event gains a `correction_prompt` field; set
  `schema_retry_nudge: false` for a bare retry with no appended turn.

### Fixed

- **Cross-platform fixes.** `stdlib_builtin_names` now uses
  `std::env::temp_dir()` instead of the hardcoded `/tmp` path; the
  ACP terminal fallback routes through `sh -c` on Unix and `cmd /C`
  on Windows instead of unconditionally shelling out to `sh`;
  `harn-serve` invokes `tokio::fs::read_to_string` for script reads
  inside async handlers so the runtime isn't blocked on large
  scripts.
- **Docs snippets (#535).** Fixed 26 failing `harn check` docs
  snippets across 13 files so `make check-docs-snippets` is clean
  again.

## v0.7.34

### Added

- **Adaptive context assembly (#530).** Adds `assemble_context`, the
  within-selection complement to `transcript_auto_compact`. Chunks
  oversized artifacts at paragraph boundaries, deduplicates across
  artifacts (exact-text or trigram-Jaccard), packs by
  recency/relevance/round-robin under a token budget, and returns a
  `{chunks, included, dropped, reasons, total_tokens, budget_tokens}`
  record. Chunk ids are content-addressed for replay determinism. A
  host `ranker_callback` plugs in custom scoring, and workflow nodes
  can declare `context_assembler: {...}` so `execute_stage_node` routes
  the stage's artifact context through the builtin without rewiring
  prompts.
- **Eval v2 replay tooling (#525).** Adds `harn trace import` to ingest
  generic `{prompt, response, tool_calls}` JSONL traces into standard
  `--llm-mock` fixtures, a `harn test --determinism` harness that
  records then replays each pipeline and diffs stdout, provider
  responses, and persisted run records, and a clarifying-question eval
  kind backed by a new `hitl_questions` run-record field populated from
  the HITL event log.

### Changed

- **Protocol-aware done sentinel guidance (#524).** Persistent no-tool
  and native-tool loops now surface bare `##DONE##` in prompts, nudges,
  mock-LLM behavior, editor hover docs, and docs snippets, while tagged
  text-tool flows continue to use `<done>##DONE##</done>`. Adds prompt
  coverage so the no-tool variant can't silently regress to the tagged
  sentinel.

## v0.7.33

### Added

- **Persona manifests and captain template packs (#460, #463, #514,
  #519).** Adds `harn persona list` / `inspect --json`, validates
  persona-manifest fields in `harn.toml`, and ships checked-in
  `merge_captain`, `review_captain`, and `oncall_captain` template
  packs with workflows, fixtures, evals, context packs, and operator
  docs.
- **Registry package discovery and typed delegation handoffs (#470,
  #461, #515, #520).** Adds a first-party TOML package registry index
  plus `harn package search` / `info`, teaches `harn add
  <registry-name>@<version>` to resolve registry versions into the
  existing Git dependency flow, and surfaces typed `HandoffArtifact`
  metadata through receipts, ACP session updates, A2A responses, and
  conformance fixtures.

### Changed

- **Docs and runtime policy refresh (#447, #473, #467, #513, #516,
  #517).** Points docs, CLI metadata, quickrefs, and redirects at
  `harnlang.com`, adds structured cancellation scopes for deadlines and
  host cancellation, and enforces connector export-effect policy around
  normalize/export calls with matching lint and contract-check
  coverage.

### Fixed

- **Package resolution and sandbox hardening (#518).** Confines direct
  imports, manifest exports, aliases, cache materialization, and Git
  temp-dir handling to package roots, makes manifest provider schema
  installation atomic under concurrent checks, and tightens macOS
  process sandbox read/write scopes.
- **Text-only agent loop completion and transcript surfaces (#521).**
  No-tool persistent loops now honor only the plain visible done
  sentinel instead of tagged `<done>` blocks, visible transcript text
  keeps private reasoning hidden while preserving thinking metadata,
  and higher-level agent config surfaces keep thinking settings wired
  through local-model stages.

## v0.7.32

### Added

- **Prompt Librarian stdlib (#313).** Adds `std/prompt_library` for reusable
  prompt fragments, TOML/front-matter `.harn.prompt` catalog loading, cached
  fragment payload metadata, tenant-scoped k-means hotspot proposals, and a
  review-queue shape for host/portal handoff.
- **`jwt_sign` crypto builtin (#454).** New
  `jwt_sign(alg, claims, private_key)` stdlib builtin produces compact
  JWT/JWS tokens signed with ES256 (P-256 PEM) or RS256 (RSA PEM) keys,
  including parser/LSP builtin metadata, spec text, highlight keyword
  generation, and conformance fixtures.
- **Orchestrator analytics stats (#304, #455).** `harn orchestrator
  stats` rolls durable trigger, predicate, DLQ, handler latency, and
  LLM cost/token telemetry into top-N summaries and persists each
  snapshot back to the `orchestrator.analytics.stats` EventLog topic
  for dashboards and audits. LLM transcript records carried inside
  trigger handlers are enriched with provider, estimated cost, and
  trigger/tenant context.
- **Generic stream trigger ingress (#280, #456).** A built-in generic
  `stream` connector normalizes unsigned HTTP ingress into stream
  trigger events, and the provider catalog advertises stream ingress
  for Kafka, NATS, Pulsar, Postgres CDC, email, and WebSocket
  providers. Native long-running broker/email consumer loops remain
  future work via provider-specific connectors or Harn connector
  overrides.
- **`harn-serve` A2A adapter (#316, #457).** `harn serve a2a` now
  runs on the shared `DispatchCore` with agent-card skill
  advertisement, task send/send-and-wait, SSE streaming and
  resubscribe, push callbacks, cancellation, shared HTTP auth, and
  optional signed agent cards. The old CLI-local A2A server is
  removed.
- **Guided connector OAuth CLI (#176, #458).** A new `harn connect`
  surface captures GitHub App installation metadata and optional
  webhook secret material, runs OAuth authorization-code setup with
  PKCE and loopback callbacks for Slack, Linear, and Notion, and
  supports `harn connect generic <provider> <url>` with OAuth
  protected-resource / authorization-server discovery, dynamic client
  registration, and resource indicators. `--list`, `--refresh`, and
  `--revoke` manage keyring-backed connector credentials.
- **Cost-aware LLM routing (#278, #459).** `llm_call` gains
  `route_policy` and `fallback_chain` handling, with `manual`,
  `always(id)`, `cheapest_over_quality(t)`, and
  `fastest_over_quality(t)` policy forms. Route decisions are recorded
  as first-class transcript events. Provider catalog metadata carries
  adapter cost/latency fields and OpenAI-compatible provider entries
  for vLLM, TGI, Groq, DeepSeek, Fireworks, DashScope, HuggingFace,
  local, and Ollama, plus existing hosted providers. The portal gains
  a Costs page and a cost report endpoint.
- **Package dependency management v1 (#469, #475).** Transitive Harn
  package dependencies are flattened from installed package manifests
  into the root `harn.lock` and `.harn/packages/`. Git package
  dependencies now require `rev` or `branch`, including
  `harn add github.com/...@ref`, with clear errors on unpinned Git
  dependencies. Transitive `path` dependencies from Git-installed
  packages are rejected so publishable packages do not depend on
  sibling checkouts.
- **Orchestrator backpressure and destination circuits (#191).**
  Webhook ingest now has global and per-provider token buckets that
  return `503` with `Retry-After` when saturated, dispatcher
  destinations open a 60-second circuit after five consecutive
  retryable failures and fail fast into DLQ while open, and
  `harn_backpressure_events_total{dimension, action}` exposes
  admission and circuit decisions for operators.
- **Connector `NormalizeResult` v1 (#464, #476).** A
  `ConnectorNormalizeResult` contract lets connector `normalize`
  exports return `event`, `batch`, `immediate_response`, or `reject`
  outcomes. The orchestrator listener enqueues zero, one, or many
  normalized events and returns connector-specified HTTP responses for
  ack-first or reject paths. The legacy direct event dict shape is
  preserved with a transition warning.
- **Connector `poll_tick` scheduler (#465, #481).** Orchestrator now
  drives Harn connector poll bindings via a `poll_tick` export,
  persists connector cursor/state, and routes returned events through
  inbox dedupe and envelope handling. Poll binding config knobs and
  the `poll_tick` export contract are documented.
- **Runtime context introspection (#482, #485).** Native
  `runtime_context()` / `task_current()` builtins expose logical Harn
  task identity and trigger/workflow/worker/agent/trace fields,
  cancellation state, and debug metadata. Parent/root/task group
  context propagates deterministically through `spawn`, `parallel`,
  `parallel each`, and `parallel settle`, including task-local context
  value snapshot inheritance. Conformance covers spawn/parallel lineage
  and context-local isolation.
- **Bounded orchestrator topic pumps (#478, #486).** Per-topic
  dispatch is now bounded by `--pump-max-outstanding` and
  `[orchestrator] pumps.max_outstanding`. The inbox pump stops reading
  and acking event-log work while admitted dispatch tasks are at
  capacity. Pump lifecycle events plus backlog, outstanding, and
  admission-delay Prometheus metrics are exported.

## v0.7.30

### Added

- **Package cache integrity tooling (#472).** `harn install --locked
  --offline` now performs reproducible cache-only installs, and
  `harn package cache list/clean/verify` exposes shared package cache
  inspection, cleanup, and lockfile hash verification.
- **Trigger budget governance and autonomy budgets (#162, #435,
  #437).** Trigger predicates now support per-call cost/token ceilings,
  hourly and daily trigger spend caps, global orchestrator budget caps,
  budget exhaustion strategies (`false`, `retry_later`, `fail`,
  `warn`), budget metrics, and `harn orchestrator inspect` budget usage.
- **Trigger action graph observability (#434).** Dispatch records now
  expose richer action graph node kinds and runtime metadata so trigger
  execution can be inspected and audited after the fact.
- **A2A push notification connector (#436).** Harn can now receive A2A
  push completion callbacks through the trigger inbox, with replay
  protections and conformance coverage for accepted and rejected flows.
- **OpenTrustGraph chain support (#420).** Added trust graph chain
  primitives, schemas, fixtures, stdlib APIs, CLI plumbing, and docs for
  recording and validating provenance-linked decisions.
- **HITL typed supervision receipts (#418).** Human-in-the-loop approval
  records now include typed signed receipt data that replays
  deterministically, with stricter lint and conformance coverage.
- **Portal and orchestrator observability surfaces (#419).** The
  portal and orchestrator inspection paths now expose richer run,
  launch, trust, and observability data, including a starter dashboard
  for operators.

### Changed

- **VM execution pipeline performance (#421-#431).** The compiler and VM
  now use typed opcodes, builtin ids, inline caches, local slots,
  indexed struct layouts, leaner value storage, and flatter opcode
  dispatch to reduce cloning, allocation, and call overhead.
- **Local and CI release workflow (#439, #440).** Local setup, hooks,
  Makefile targets, CI, and the release scripts now prefer nextest where
  available, lint GitHub Actions, support merge-queue-safe release
  branches, and document the two-PR release flow.
- **Conformance and test isolation (#438).** Conformance runs are
  faster and test execution disables real LLM calls by default unless a
  test explicitly opts in.

### Fixed

- **Line-leading nil-coalescing continuations.** Expressions can now
  continue on a following line that starts with `??`, equality, or
  comparison operators, matching the formatter and tree-sitter grammar.
- **HTTP mock call headers (#432).** Mocked HTTP calls now record
  request headers, making replay and conformance assertions cover the
  full call shape.
- **Webhook dedupe retention coverage (#417).** Webhook dedupe handling
  has stronger regression coverage for retained deliveries and duplicate
  suppression.

## v0.7.29

### Fixed

- **Stable script source directories.** `harn run` now stores source
  directories as absolute paths before exposing them through
  `source_dir()`. Scripts can safely derive sibling paths from
  `source_dir()` even when they `cd` elsewhere before shelling out to a
  nested `harn` command.

## v0.7.28

### Fixed

- **Crates.io `harn-cli` installation.** `harn-modules` now packages
  crate-local copies of the runtime stdlib `.harn` sources instead of
  using workspace-relative `include_str!` paths into `harn-vm`, so
  `cargo install harn-cli --locked` can compile the published crate from
  crates.io. The module graph mirror now covers the full runtime stdlib
  import surface, including `std/hitl`, waitpoint/monitor modules, and
  connector stdlib modules.
- **Release package verification.** Added `scripts/verify_crate_packages.sh`
  to package `harn-modules`, inspect the extracted crate archive, compare
  its stdlib mirror with `harn-vm`, compile the extracted package, and
  package `harn-cli`. The per-crate publish fallback list now includes
  every publishable workspace crate, including `harn-modules`.

## v0.7.27

This release rounds out the trigger + orchestration surface with two
new moat primitives — durable `monitor.wait_for` with push-driven
wakeups, and first-class stream-trigger manifests for Kafka / NATS /
Pulsar / Postgres CDC / email / WebSocket connectors — alongside
ACP `session/fork`, the manifest-driven `harn orchestrator deploy`
helper, and a reproducible VM microbenchmark suite with a baseline
table. Also fixes an `EventLog::subscribe` scheduling bug where the
detached forwarder thread was invisible to tokio paused-time
auto-advance and could race with auto-advanced timers under load.

### Added

- **ACP `session/fork` support (#319, #364).** Runtime transcripts can
  now be forked in place, with fork metadata notifications wired
  through the ACP stdio integration. ACP session ids are bound to the
  runtime session store so prompts and forks operate on the same
  transcript state.
- **`std/monitors` `wait_for` primitive (#303, #405).** A durable
  monitor-wait builtin that records `monitor_wait_started` /
  `monitor_wait_matched` / `monitor_wait_timed_out` events to the event
  log, supports push-driven wakeups from the trigger inbox with
  poll-interval fallback, and replays recorded terminal results during
  dispatch replays for deterministic reruns.
- **`harn orchestrator deploy` helper (#188, #414).** Generate and run
  manifest-driven orchestrator deploys against Fly, Railway, or Render
  from the CLI, with provider-specific starter configs shipped under
  `deploy/` for quick onboarding.
- **VM microbenchmark suite (#402, #415).** A deterministic Harn
  fixture set under `perf/vm/` plus `scripts/bench_vm.sh` and a
  `make bench-vm` target, with a `perf/vm/BASELINE.md` baseline table
  the script can diff against to catch VM performance regressions.
- **Stream trigger manifest primitives (#416).** First-class manifest
  support for streaming providers — Kafka, NATS, Pulsar, Postgres CDC,
  email, and WebSocket — with package-level schema validation,
  trigger-inbox plumbing, and a `stream-fan-in` example under
  `examples/triggers/`.

### Fixed

- **Event-log subscription scheduling.** `EventLog::subscribe` now
  forwards history and live events on a tokio task instead of a
  detached `std::thread::spawn` + `futures::executor::block_on`. The
  old implementation was invisible to tokio's paused-time auto-advance
  and raced with auto-advanced timers under load; the tokio task
  participates in runtime scheduling and is cancelled cleanly on
  shutdown.

## v0.7.26

This release lands the full groundwork slate for epic #350 — the
pure-Harn connectors pivot. Package management, the Harn-backed
connector contract, HTTP retries with `Retry-After`, the standard
encoding helpers, and first-class `bytes` + raw inbound bodies now
ship together so external connector repos can be written entirely
in Harn.

### Fixed

- **Native-tool stages can now fail closed or one-shot text fallback,
  and they expose structured fallback/retry metadata (#229).**
  `agent_loop`, `sub_agent_run`, and workflow `model_policy` now accept
  `native_tool_fallback: "allow" | "allow_once" | "reject"` for
  native-tool stages. Harn also records
  `native_text_tool_fallbacks`,
  `native_text_tool_fallback_rejections`, and
  `empty_completion_retries` in stage trace summaries / observability so
  eval tooling can distinguish tolerated provider recovery from the
  intended native-tool contract.

### Added

- **`project_fingerprint()` now returns a normalized repo profile for
  autonomous personas (#218).** The shallow detector now exposes stable
  primary tags for package manager, test runner, build tool, VCS, and CI
  provider alongside the existing language/framework signals, with
  conformance coverage for representative Rust, Swift, Node, Python, Go,
  mixed, and empty-directory shapes.
- **Durable workflow message/runtime control surface (#302).** Harn now ships
  persisted workflow mailbox builtins (`workflow.signal`, `workflow.query`,
  `workflow.publish_query`, `workflow.update`, `workflow.receive`,
  `workflow.respond_update`, `workflow.pause`, `workflow.resume`,
  `workflow.status`, and `workflow.continue_as_new` plus top-level
  `continue_as_new`) backed by `.harn/workflows/<workflow_id>/state.json`.
  `std/agents` workflow sessions now preserve `workflow_id`, and ACP/A2A expose
  matching workflow control methods so external callers can signal, query,
  update, pause, resume, and roll generations forward through the same durable
  runtime state.
- **`harn serve mcp` workflow adapter for exported `pub fn` entrypoints
  (#293).** The shared `harn-serve` core now ships its first transport
  adapter: MCP. Any `.harn` module with exported `pub fn` entrypoints
  can now be served directly as MCP tools with input/output schemas
  derived from Harn types, cooperative cancellation, out-of-band
  progress notifications, stdio + Streamable HTTP + legacy SSE
  transports, and HTTP auth hooks for API-key / HMAC deployments.
- **Git-backed package manager v0 (#345, #355).** Typed lockfile with
  content hashes, `harn add/install/update/remove/lock` commands, shared
  cache, ref resolution, frozen/refetch flows, and import resolution
  that materializes `.harn/packages/` without auto-merging hooks,
  triggers, or LLM config from installed manifests. Groundwork for the
  pure-Harn connectors pivot (#350) — external connector repos can now
  be consumed with `harn add <git-url>`.
- **Harn-backed connector modules (#346, #356).** Manifest-driven
  `[connectors.harn.<provider>]` overrides, a dedicated Harn connector
  adapter/runtime, and connector-only builtins for secrets, event-log
  writes, and custom metrics wired through orchestrator ingress and
  outbound client paths. Enables connectors to be authored in pure Harn
  and shipped from external repos (#350).
- **HTTP builtins support per-request retry policy and `timeout_ms`
  (#348, #353).** `http_get` / `http_post` / friends accept canonical
  `timeout_ms` and `retry: {max, backoff_ms}` options while preserving
  the legacy flat aliases, default retries now cover `408`, `429`,
  `500`, `502`, `503`, and `504` for idempotent methods, `Retry-After`
  is honored on `429` / `503`, and `http_mock` can script response
  sequences through `responses: [...]` for conformance-friendly retry
  tests.
- **Stdlib encoding builtins (#349, #352).** `base64url_encode` /
  `base64url_decode`, `base32_encode` / `base32_decode`, and
  `hex_encode` / `hex_decode` join the existing `base64_*`, `url_*`
  helpers in the crypto stdlib. Supports the pure-Harn connectors pivot
  (#350) by giving Harn code first-class access to the encodings that
  webhook signatures and typical REST APIs use.
- **First-class `bytes` runtime value + trigger raw-body access (#347,
  #354).** New `bytes` type with `bytes_from_string` / `bytes_to_string`
  / `bytes_from_hex` / `bytes_to_hex` / `bytes_from_base64` /
  `bytes_to_base64` / `bytes_len` / `bytes_concat` / `bytes_slice` /
  `bytes_eq` stdlib helpers, file-IO helpers that round-trip raw
  buffers, and inbound `TriggerEvent.raw_body` exposure so signature
  verification doesn't need to round-trip bytes through strings. Final
  epic-#350 groundwork piece: pure-Harn connectors can now own
  signature verification end-to-end.
- **Deterministic OCR stdlib and typed-tool docs (#311).** Added
  `vision_ocr(...)` plus `import "std/vision"` for structured OCR over
  image paths or inline payloads, with token/line/block output and
  `audit.vision_ocr` event-log records that capture the canonical input
  plus output. Docs now show how to wire deterministic stdlib logic into
  typed `agent_loop(...)` tools instead of inventing bespoke host tools
  for math, regex, strings, crypto, and OCR.

## v0.7.25

### Added

- **`hmac_sha256`, `hmac_sha256_base64`, and `constant_time_eq` stdlib
  builtins (groundwork for #350).** Lifts the existing private
  `hmac_sha256` helper out of `connectors/hmac.rs` into the stdlib so
  pure-Harn connector implementations can verify webhook signatures
  without re-implementing crypto. `constant_time_eq` wraps
  `subtle::ConstantTimeEq` so scripts can compare signatures without
  leaking byte positions through timing. Covered by RFC 4231 and
  GitHub's documented webhook test vectors.
- **`render_string(template, bindings?)` for inline prompt/codegen
  templates (#357).** The stdlib template engine can now render
  triple-quoted inline strings with the same `{{ if }}`, `{{ for }}`,
  filters, includes, whitespace trimming, and error behavior as the
  existing file-backed `render(...)` / `render_prompt(...)` helpers,
  so one-file-loadable libraries no longer need to ship a separate
  `.prompt` asset or reimplement the template engine in pure Harn.

- **`trust_query(filters)` now supports `limit` and `grouped_by_trace`
  (#338).** Trust-graph queries can now cap results to the newest N
  matching records server-side and optionally return
  `{trace_id, records}` buckets for timeline UIs that need bounded
  polling without regrouping the full history client-side. The same
  filters are now wired through the stdlib builtin, `harn trust query`,
  and `harn mcp serve`'s `harn.trust.query` tool.

### Fixed

- **Orchestrator no longer SIGTERM-races under parallel test load
  (#344).** `harn orchestrator serve` now installs tokio Unix signal
  streams before logging its `HTTP listener ready` line, closing the
  window where a supervisor could observe readiness and send SIGTERM
  before the handler was wired up.
- **`harn-cli` unit-test isolation on event-log globals (#351).** The
  shared `HARN_STATE_DIR` / `HARN_EVENT_LOG_*` environment variables and
  process-global event-log thread-local are now gated by a
  `lock_harn_state()` helper so tests stop observing stale dedupe state
  from earlier fixtures under `cargo test` parallelism.

### Internal

- **Gitignore ephemeral conformance `.harn/` artifacts.** SQLite
  event-log files (`events.sqlite`, `events.sqlite-shm`,
  `events.sqlite-wal`) and checkpoint directories dropped by
  conformance test runs are no longer surfaced as untracked files.

## v0.7.24

### Fixed

- **`project_fingerprint` no longer leaks its fixture tree into the repo
  root (#330).** The conformance test now builds its synthetic repo under
  a temp directory and cleans it up automatically, so documented
  `harn test conformance` runs stop leaving a stray
  `project_fingerprint_repo/` behind.

### Added

- **Native Linear connector with `harn connect linear` (#339, harn#173).**
  Harn now ships a built-in `LinearConnector` with signed inbound webhook
  normalization, typed payload variants, typed `updatedFrom` issue diffs,
  and outbound GraphQL helpers. The new `harn connect linear` CLI
  registers webhooks end-to-end, and an optional health monitor probes a
  configured health URL and auto-re-enables webhooks via
  `webhookUpdate(enabled: true)` after a healthy streak. Covered by
  stdlib, docs, and conformance fixtures.

- **Notion hybrid connector (#334, harn#174).** Replaces the
  webhook-only Notion plumbing with a production-capable hybrid
  connector: webhook handshake capture, signed webhook verification,
  dedupe, a polling fallback with persisted high-water marks, snapshot
  diffing, and 429 backoff. Ships outbound stdlib helpers for
  `get_page`, `update_page`, `append_blocks`, `query_database`,
  `search`, `create_comment`, and `api_call`; the orchestrator listener
  now routes Notion ingress through the connector and emits the required
  handshake response, and `harn doctor` surfaces captured Notion
  verification tokens.

- **Slack Events API connector expansion (#332, harn#171).** Inbound
  Slack event typing and normalization now cover the core Events API
  surface across runtime, trigger schema, and stdlib, with outbound
  helpers for `open_view`, `user_info`, and a generic `api_call`. The
  listener path adds Slack delivery metrics plus
  `x-slack-no-retry: 1` on permanent client errors, and a sample Slack
  app manifest ships alongside focused Rust tests and conformance
  coverage for inbound events and 3s acknowledgement behavior.

- **`hitl_pending(filters)` exposes typed pending HITL inbox rows (harn#333).**
  Harn scripts can now read merged pending requests from `hitl.questions`,
  `hitl.approvals`, `hitl.dual_control`, and `hitl.escalations` through the
  event log without reaching into SQLite directly.

- **Expose `agent_session_current_id()` as a public stdlib builtin
  (#318).** Handlers and subscribers can now read the innermost active
  agent session id directly, which makes it easier to compose
  `agent_session_snapshot`, `agent_session_fork`, and
  `agent_session_trim` against the currently executing session without
  threading the id through every call.

## v0.7.23

### Changed

- **`worker://` trigger dispatch now ships as a durable EventLog-backed
  queue (harn#182).** The dispatcher now enqueues worker jobs under
  `worker.<queue>`, tracks claim/ack/TTL state in companion claim
  topics, records handler results under `worker.<queue>.responses`, and
  exposes queue inspection/drain/purge through
  `harn orchestrator queue {ls,drain,purge}`. Queue priority honors the
  manifest's scalar `priority = "high" | "normal" | "low"` by default,
  with age-based promotion for older normal jobs and event-header
  overrides when callers need per-delivery priority.

- **Agent session forks now record ancestry (#320).** `agent_session_fork`
  and `agent_session_fork_at` populate the new `agent_session_ancestry(id)`
  query so replay/eval tooling can trace the parent→child chain across
  forks, with coverage in `conformance/tests/agents/agent_sessions_ancestry`.

- **Bundled secret scanning now ships in both stdlib and `harn mcp serve`
  (#309).** Harn now exposes `secret_scan(content)` for in-process
  scans plus the `harn.secret_scan` MCP tool for agent-loop PR gates.
  Findings are redacted, tagged with detector-pack provenance, and
  mirrored into `audit.secret_scan` so future trust-graph consumers can
  reason about PR hygiene without persisting raw credentials. The lint
  crate also warns when handlers call `git::push_pr` without a prior
  `secret_scan(...)` in the same flow.

- **Bulk trigger replay/cancel now use shared event-log filters and
  durable control records (#308).** `harn trigger replay` gained a
  `--where` bulk mode with Harn-expression filtering over normalized
  `event` / `binding` / `attempt` / `outcome` / `audit` records, plus
  `--dry-run`, `--progress`, and `--rate-limit`. Harn also now ships
  `harn trigger cancel` for single-event or filtered bulk cancellation.
  Cancel requests append to `trigger.cancel.requests`, long-running
  local handlers poll and honor those requests, and both bulk replay and
  cancel append operator metadata to `trigger.operations.audit` for
  future portal/MCP surfaces.

- **`project_enrich(...)` now surfaces repo operator metadata (#219).**
  The deterministic enrichment evidence now includes a `ci` block with
  parsed GitHub Actions workflows, hook stage summaries
  (`.githooks`/`pre-commit`/`lefthook`/`husky`), package manifest + lockfile
  presence, CI cache/tooling hints, and merge-policy signals from
  CODEOWNERS/CONTRIBUTING plus GitHub branch protection when `gh` is
  authenticated. Workflow/hook/policy files are also prioritized in the
  bounded prompt context so merge-captain / deploy-captain style personas
  see operator conventions without guessing.

- **Manifest triggers now carry `[[triggers]].autonomy_tier`.** Trigger
  registrations can declare `shadow`, `suggest`, `act_with_approval`,
  or `act_auto`, and handlers now receive the effective tier at
  runtime through `handler_context().autonomy_tier`.

- **Split trigger inbox envelopes from durable dedupe claims (#243).**
  Dispatcher envelopes now append to `trigger.inbox.envelopes` while
  `InboxIndex` persists TTL-bound claim records under
  `trigger.inbox.claims`, which removes the steady-state startup scan
  over all historical envelopes. Harn v0.7.23 soft-reads legacy
  `trigger.inbox` records on startup so existing orchestrator event
  logs keep working while new writes land only on the split topics.

- **Two-tier skill loading for large registries (#217).** Filesystem
  `SKILL.md` discovery now requires a compact `short:` frontmatter card
  and keeps only the always-loaded metadata in the startup `skills`
  registry. Full skill bodies move behind lazy loading via the new
  `load_skill("name")` builtin, while `agent_loop`'s runtime
  `load_skill({ name })` tool now hydrates those bodies on demand from
  the same registry.

- **Orchestrator restart no longer auto-replays stranded inbox envelopes
  (harn#242, backward-incompatible).** `harn orchestrator serve` used to
  silently re-dispatch historical `trigger.inbox` entries that had no
  matching `trigger.outbox` history, which could re-fire webhook/a2a
  handlers users never intended to replay. Restart now leaves those
  envelopes stranded, surfaces them via `harn orchestrator queue`, emits
  `orchestrator.lifecycle/startup_stranded_envelopes` with a count, and
  requires an explicit `harn orchestrator recover --envelope-age <duration>`
  flow (`--dry-run` to inspect, `--yes` to actually replay).

- **Primary Harn docs site moved from `harn.burincode.com` to
  `harnlang.com`.** The burin-code-subdomain was always a stopgap;
  `harnlang.com` reflects Harn's identity as a standalone programming
  language + runtime (precedent: `rust-lang.org`, `elixir-lang.org`).
  `harn.burincode.com` continues to 301 → `harnlang.com` for 12+
  months to preserve external links (crates.io metadata, blog posts,
  cached search results). References in CLI help text, README, docs,
  Cargo crate metadata, mdBook site metadata, skill registry examples,
  CLAUDE.md, and conformance fixtures all point at the new domain.

### Fixed

- **`trigger_replay` now recovers when the recorded binding version has
  already been GC'd after manifest hot-reloads (harn#248).**
  Both the stdlib replay path and `harn trigger replay` now share a
  registry helper that falls back to lifecycle-history resolution and
  emits a structured `replay.binding_version_gc_fallback` warning with
  the trigger id, recorded version, event timestamp, and resolved
  version.

- **Webhook inbox dedupe is active again (#223).** The async inbox-claim
  step now runs after inbound webhook normalization but before the
  event is appended to the pending log, so duplicate GitHub-style
  deliveries are dropped instead of being enqueued twice. This
  replaces the temporary `block_on` bridge inside
  `GenericWebhookConnector::normalize_inbound` with a proper async
  post-processing step on the dispatch path. The cron connector keeps
  its existing async dedupe path and explicitly avoids double-claiming
  the same inbox key.

- **Replay-scoped `HARN_REPLAY` no longer races across concurrent
  dispatches (harn#244).** Replay handlers still observe
  `env_or("HARN_REPLAY", ...) == "1"` and replay-spawned subprocesses
  still inherit `HARN_REPLAY=1`, but the runtime no longer flips the
  process-global env var for the full async dispatch lifetime. Replay
  detection is now scoped to the specific in-flight dispatch, so
  overlapping replays and tests that pre-set `HARN_REPLAY` no longer
  corrupt each other’s value restoration.

- **Trigger inbox shutdown race could silently drop dequeued webhook
  events (harn#241).** The dispatcher and orchestrator inbox pump no
  longer detach `dispatch_inbox_envelope(...)` into fire-and-forget
  local tasks during shutdown. Once an event is read from
  `trigger.inbox`, drain now waits for that dispatch attempt to either
  record its outbox outcome or observe cancellation instead of letting
  SIGTERM exit with the envelope stranded between inbox and outbox.

- **Bounded orchestrator pump drain on shutdown (harn#240).** The
  orchestrator no longer tries to drain an unbounded pending/cron/inbox
  backlog during SIGTERM/SIGINT. `harn orchestrator serve` now applies a
  configurable per-pump shutdown bound from
  `[orchestrator].drain.{max_items,deadline_seconds}` or
  `--drain-max-items` / `--drain-deadline`, emits
  `orchestrator.lifecycle` `drain_truncated` when backlog remains, and
  resumes truncated pump backlog on the next start from a durable pump
  cursor instead of skipping pre-existing source-topic events.

- **Flaky cwd-mutating test collisions (#204).** Added a shared-process
  cwd mutex so parallel `cargo test` no longer observes mid-test cwd
  swaps. `check_manifest_reports_loaded_triggers` and
  `run_tests_uses_file_parent_as_execution_cwd_and_restores_shell_cwd`
  no longer flake on CI.

- **DLQ topic split-brain between dispatcher and CLI.** The trigger
  dispatcher at `harn_vm::triggers::dispatcher::TRIGGER_DLQ_TOPIC`
  writes to `trigger.dlq`, but the orchestrator CLI readers and the
  `trigger_inspect_dlq()` stdlib entrypoint were reading from
  `triggers.dlq` (trailing `s`). Both paths now agree on the
  `trigger.dlq` topic name, so `harn orchestrator dlq` and the
  stdlib-driven replay workflow actually surface DLQ entries the
  dispatcher has written.

- **Flaky `replay_dispatch_emits_replay_chain_edge_and_headers` test
  under parallel `cargo test` (harn#244 band-aid).** The replay path
  mutates the process-wide `HARN_REPLAY` env var via `ReplayEnvGuard`
  and a sibling test manipulates the same var from test-level setup
  under its own `replay_env_lock()`. Both replay-driving tests now
  take the same lock; the structural task-local fix is still tracked
  as harn#244.

- **Structured-output schema contract for OpenRouter Gemini (#208,
  closes #206).** Schema-mode `llm_call(...)` no longer returns a
  success envelope with `data == nil` after prose-only or
  non-parseable responses. Missing parseable JSON now counts as a
  schema failure and feeds `schema_retries`. Preserves bare retries
  when `schema_retry_nudge: false`. Broadens JSON extraction to
  recover structured output from tagged prose and canonical public
  output blocks. Maps Harn's `thinking` option onto OpenRouter's
  `reasoning` request surface (no more "thinking unsupported"
  warnings there).

### Added

- **Native Linear connector + `harn connect linear` (#173).** Harn now ships a
  first-class `LinearConnector` with signed webhook ingestion
  (`Linear-Signature` + `webhookTimestamp` replay protection), typed payloads
  for issue/comment/project/cycle/customer updates, typed `updatedFrom`
  issue-change decoding, optional webhook health probing with automatic
  re-enable attempts, outbound GraphQL helpers through `std/connectors/linear`,
  and a `harn connect linear` CLI that creates webhooks from
  manifest-derived resource types.

- **Shared `harn-serve` dispatch core (harn#301).** New `harn-serve`
  workspace crate introduces a transport-agnostic adapter boundary,
  shared API-key/HMAC/OAuth auth handling, an in-memory replay cache,
  export-catalog discovery from `pub fn` exports with JSON schema
  metadata, and shared dispatch plumbing for cancellation, trust-graph
  context, and OpenTelemetry parent propagation. Transport-specific
  serve implementations can now delegate these concerns instead of
  reimplementing them.

- **Expression-keyed trigger flow control for manifest bindings (harn#307).**
  `[[triggers]]` now supports top-level `concurrency`, `throttle`,
  `rate_limit`, `debounce`, `singleton`, `batch`, and keyed `priority`
  tables. Keys compile against the typed `TriggerEvent` surface, flow-control
  decisions emit EventLog records under dedicated `trigger.<gate>.*` topics,
  batch dispatch attaches coalesced members on `event.batch`, and legacy
  `budget.max_concurrent` now warns and normalizes to
  `concurrency = { max = N }`.

- **Cryptographic provenance for Harn skills.** Added `harn skill key
  generate`, `harn skill sign`, `harn skill verify`, `harn skill trust
  add`, and `harn skill trust list` for Ed25519-based detached
  signatures over `SKILL.md`. Skill manifests now support
  `require_signature` and `trusted_signers`, projects can configure a
  `signer_registry_url`, runtime `load_skill(...)` can require signed
  skills via call arg or `HARN_REQUIRE_SIGNED_SKILLS=1`, and every load
  attempt emits a `skill.loaded` trust record into the transcript.

- **`harn mcp serve` exposes orchestrators as MCP servers.**
  Added a new orchestrator-backed MCP server command that serves over stdio and
  HTTP, exposes trigger fire/list/replay, queue + DLQ inspection/retry,
  dispatcher inspection, manifest/event/DLQ resources, and a placeholder trust
  query surface. Tool calls now append `observability.action_graph` entries with
  MCP client identity so external MCP clients can drive Harn without a custom
  adapter layer.

- **Typed human-in-the-loop stdlib primitives.** Harn now ships VM-backed
  `ask_user`, `request_approval`, `dual_control`, and `escalate_to`
  builtins with durable `hitl.*` event-log records, replay-safe
  resolution from recorded responses, a shared `std/hitl` type catalog,
  and host ingress via `harn.hitl.respond` on the ACP/MCP bridge. Added
  HITL unit/conformance coverage, `docs/src/hitl.md`, quickref/spec
  documentation, signed approval timestamp receipts that replay
  deterministically, strict-mode lint coverage for discarded approval
  records, and `harn orchestrator resume <request_id>` for manual
  escalation acceptance.

- **LLM-gated trigger predicates with replay-safe cost governance
  (harn#161).** `[[triggers]]` and `trigger_register(...)` now accept
  `when = ...` plus `when_budget = {max_cost_usd, tokens_max, timeout}`
  so typed predicates can call `llm_call(...)` before handler
  dispatch. Predicate spend is tracked against the trigger's UTC-day
  `budget.daily_cost_usd`; overruns emit
  `predicate.budget_exceeded` / `predicate.daily_budget_exceeded` and
  fail closed. Predicate `llm_call(...)` results are cached in the
  request cache plus per-event `trigger.inbox` records so replay stays
  deterministic, `predicate.evaluated` now emits cost/token/cache
  metadata, action-graph observability includes a
  `trigger_predicate` node kind, and three consecutive predicate
  failures open a five-minute circuit breaker with operator-visible
  warnings.

- **OpenTelemetry tracing and metrics for orchestrator dispatch flow (#184).**
  Added orchestrator observability bootstrap in `harn orchestrator serve` with
  `HARN_OTEL_ENDPOINT`, `HARN_OTEL_SERVICE_NAME`, and `HARN_OTEL_HEADERS`
  propagation, plus OTel-enabled `ingest` and `dispatch` spans that share an
  end-to-end trace id and include dispatch outcome attributes (`result.status`,
  `result.duration_ms`). Listener ingest now records a `trace_id` on each pending
  trigger payload and propagates ingest span context into dispatcher via
  `otel_parent_span_id`, so pending work is linked end-to-end. Added Prometheus
  counters (`dispatch_succeeded_total`, `dispatch_failed_total`, `inbox_duplicates_total`,
  `retry_scheduled_total`) and `GET /metrics` on the listener. Added an
  integration test asserting OTLP span emission with shared trace ids across ingress
  and dispatch hops.

- **Trust graph runtime, CLI, stdlib, and OpenTrustGraph draft schema.**
  Every terminal trigger dispatch now appends a `TrustRecord` to
  `trust.graph` plus `trust.graph.<agent_id>`, `std/triggers` now
  exposes `handler_context()`, `trust_record(...)`, and
  `trust_query(...)`, and the CLI now includes `harn trust query`,
  `harn trust promote`, and `harn trust demote`. Added the
  `spec/opentrustgraph.md` draft plus trust-graph docs and conformance
  coverage.

- **First-class Slack Events connector (#239).** Harn now ships a
  `slack` connector that handles Slack's URL-verification challenge
  response inline, normalizes `event_callback` payloads into
  `TriggerEvent`s keyed by `team_id:event_id`, and verifies
  `X-Slack-Signature` / `X-Slack-Request-Timestamp` with a 5-minute
  skew tolerance before enqueueing. Registered as the default
  connector for provider `slack` in
  `ConnectorRegistry::with_defaults()` with listener-route
  `signature_mode: SignatureMode::Unsigned` (Slack uses its own
  signing scheme). Idle-GC the known dedupe index entries on a
  24-hour TTL.

- **`harn orchestrator {inspect, fire, replay, dlq, queue}` CLI
  commands (#185).** Implemented the placeholder orchestrator
  subcommands that used to error with `not implemented`. `inspect`
  dumps the orchestrator state snapshot + trigger bindings, `fire`
  enqueues a synthetic `TriggerEvent` for a given trigger id, `replay`
  re-dispatches a historical event through the trigger dispatcher
  (complementary to `harn trigger replay`, which works against a
  standalone EventLog), `dlq` lists dead-letter entries, and `queue`
  shows the pending-queue head. Orchestrator run fixtures cover each
  command against a live `harn orchestrator serve`. Also stabilized
  `orchestrator_inbox_dedupe` by awaiting `activated connectors:
  cron(1)` instead of the HTTP listener ready line (closes harn#230
  flake).

- **Bridge-backed host tool discovery (#216).** Bridge sessions now
  expose `host_tool_list()` and `host_tool_call(name, args)` stdlib
  entry points plus matching parser signatures. `host_tool_list()`
  returns the full catalog including per-tool schemas — scripts
  call it once and consult the result instead of needing a separate
  `describe` call. Harn programs running inside burin-code can
  enumerate the host's editor tools, read their schemas, and invoke
  them through the bridge without the host needing to pre-inject a
  static catalog.

- **`harn trigger replay <event-id>` CLI command (#222).** Added a
  top-level replay CLI that works directly against an EventLog
  snapshot without requiring an orchestrator to be running. Supports
  `--diff` drift reporting (structured JSON comparing original vs.
  replay result) and `--as-of <timestamp>` historical binding
  resolution via trigger lifecycle history. Sets `HARN_REPLAY=1`
  during replay dispatch so runtime nondeterminism (e.g. `uuid()`,
  timestamps) can fall back to recorded values when handlers
  cooperate. Replay now also falls back automatically to the binding
  active at the recorded event timestamp when the recorded binding
  version is no longer resolvable. Complements the in-process
  `trigger_replay(...)` stdlib and the orchestrator-scoped `harn
  orchestrator replay`.

- **Hardened orchestrator shutdown drain (#183).** SIGTERM/SIGINT now
  stops new HTTP traffic, drains pending/cron/inbox work, and waits
  for in-flight dispatcher tasks up to a configurable deadline. Added
  connector + event-log flush hooks so persisted connector boundaries
  and durable event-log state land before exit, plus
  `orchestrator.lifecycle` `draining` / `stopped` events with drain
  counts. Extends orchestrator integration coverage for mid-dispatch
  SIGTERM handling.

- **Distroless multi-arch orchestrator container (#186).** Added a root
  `Dockerfile` for `harn orchestrator serve` with a Rust 1.95 builder,
  distroless `cc` runtime, non-root UID `10001`, and a Docker healthcheck
  that probes `GET /health`. `harn orchestrator serve` now accepts
  container-friendly `--manifest` / `--listen` aliases plus
  `HARN_ORCHESTRATOR_*` env defaults, `.dockerignore` prunes bulky build
  outputs from the image context, `a2a-push` listener routes can enforce
  bearer API keys or canonical-request HMAC auth via
  `HARN_ORCHESTRATOR_API_KEYS` and `HARN_ORCHESTRATOR_HMAC_SECRET`, and
  the release-tag workflow now builds and pushes `linux/amd64` +
  `linux/arm64` images to `ghcr.io/burin-labs/harn`.

- **Real `a2a://...` trigger dispatch in the runtime (#181).** The
  dispatcher now resolves `a2a://host[:port]/path` handlers through the
  target agent card, requires a confirmed-unique JSON-RPC endpoint,
  posts the `TriggerEvent` envelope over `a2a.SendMessage`, and returns
  either the inline remote result or a pending task handle payload.
  A2A card discovery now prefers HTTPS, only falls back to cleartext
  after an HTTPS connect-refused failure, and rejects agent cards whose
  declared URL authority does not match the requested target. Cleartext
  A2A discovery / dispatch now also requires explicit
  `allow_cleartext = true` on the trigger binding. The broader
  host-allowlist follow-up remains deferred.
  Dispatcher retries / DLQ behavior now apply to remote A2A attempts the
  same way they already applied to local handlers. Persisted
  observability adds `a2a_hop` nodes and `a2a_dispatch` edges with
  propagated `trace_id` and `target_agent` context. Adds dispatcher unit
  coverage for inline + pending A2A responses and a conformance fixture
  that exercises `trigger_register(...)` / `trigger_fire(...)` against a
  live `harn serve` receiver.

- **Trigger event replay now routes through the dispatcher (#166).**
  `trigger_replay(...)` no longer uses the local shallow stub. The
  stdlib now looks up historical events from `triggers.events`,
  re-dispatches them through `harn_vm::triggers::Dispatcher`, preserves
  `replay_of_event_id` on the returned `DispatchHandle`, resolves the
  pending stdlib DLQ summary entry when a replay succeeds, and carries
  replay metadata into derived run observability so the portal can show
  a `replay_chain` link back to the original event. Dynamic
  `trigger_register(...)` configs now accept a minimal stdlib retry
  override surface: `{max, backoff: "svix" | "immediate"}`.

- **Handler dispatcher with URI routing, retries, cancellation, and
  streaming trigger action-graph updates (#159).** Added
  `harn_vm::triggers::Dispatcher` with EventLog-backed
  `trigger.inbox` / `trigger.outbox` / `trigger.attempts` / `trigger.dlq`
  topics, local handler execution against the live trigger registry,
  manifest-driven retry policy normalization (`Svix`, `Linear`,
  `Exponential`), cooperative shutdown propagation into in-flight local
  handler VMs, and new dispatcher lifecycle records on
  `triggers.lifecycle`. Closes the T-10 deferral for
  `dispatch` / `retry` / `dlq` action-graph nodes and
  `retry` / `dlq_move` edges on the local-handler path. Follow-up work
  since extended the remote side with `a2a_hop` / `a2a_dispatch`; only
  `worker://...` remains deferred to O-05.

- **Durable trigger inbox dedupe on top of the shared EventLog (#160).**
  `harn_vm::triggers::InboxIndex` now persists dedupe claims on the
  `trigger.inbox` topic, rehydrates them on restart, honors per-trigger
  `retry.retention_days` TTLs, and preserves a process-local hot-key cache
  for repeated deliveries. The live cron connector now claims
  `(binding_id, dedupe_key)` before it appends `connectors.cron.tick`, so a
  crash after emit but before cron state persistence no longer duplicates the
  same logical tick on restart. Added connector metrics snapshots for inbox
  claims/duplicate rejections, manifest/docs coverage for retention, and
  restart coverage via both mock-clock VM tests and an orchestrator fixture
  under `conformance/fixtures/triggers/inbox_dedupe_restart`.

- **Reusable trigger-system test harness with 12 MVP fixtures (#165).**
  Added `harn_vm::triggers::test_util` with a shared mock clock,
  recording connector sink/registry, and fixture runner that exercises
  cron scheduling, webhook HMAC verification, retry backoff, DLQ +
  replay, dedupe, rate limiting, cost guards, crash recovery,
  hot-reload preservation of in-flight work, multi-tenant stubs, and
  dead-man alerts. Exposed the same core harness to Harn scripts via the
  new `trigger_test_harness(...)` builtin and added conformance fixtures
  under `conformance/tests/triggers/`.

- **GitHub App connector with signed webhooks + installation-auth outbound
  helpers (#170).** `harn_vm::connectors::GitHubConnector` now plugs into the
  shared `Connector` + `ConnectorRegistry` runtime, verifies inbound
  `X-Hub-Signature-256` webhook deliveries through the shared HMAC helper, and
  narrows GitHub payloads into typed `GitHubEventPayload` variants for
  `issues`, `pull_request`, `issue_comment`, `pull_request_review`, `push`, and
  `workflow_run`. Outbound calls authenticate as a GitHub App installation with
  cached installation tokens refreshed before the one-hour expiry and re-minted
  on `401`, route through the shared `RateLimiterFactory`, and ship Harn
  stdlib wrappers for `comment`, `add_labels`, `request_review`, `merge_pr`,
  `list_stale_prs`, `get_pr_diff`, and `create_issue`. Includes conformance
  coverage against a mock GitHub server plus manual-setup docs at
  `docs/src/connectors/github.md`. Registered as the default connector for
  provider `github` in `ConnectorRegistry::with_defaults()`, replacing the
  generic webhook receiver previously wired up by the provider catalog.

- **`harn orchestrator serve` CLI scaffold (#209, closes #178).** Added
  a new `harn orchestrator` command family with a real `serve`
  subcommand plus placeholder `inspect`, `replay`, `dlq`, and
  `queue` subcommands. `serve` now loads `harn.toml`, boots a
  single-tenant orchestrator VM, installs the shared EventLog under
  `--state-dir`, resolves the active secret-provider chain, collects
  manifest triggers, activates placeholder connectors per manifest
  provider, writes an orchestrator state snapshot, and idles until
  SIGTERM for scaffolded graceful shutdown. Multi-tenant remains an
  explicit `O-12 #190` stub.
- **Axum-based orchestrator HTTP listener with TLS, origin guards,
  and body limits (#179).** `harn orchestrator serve` now binds a
  real Axum listener on `--bind`, optionally serves HTTPS with
  `--cert` + `--key`, enforces `[orchestrator].allowed_origins`
  and `[orchestrator].max_body_bytes`, registers HTTP routes for
  manifest `webhook` and `a2a-push` triggers, normalizes inbound
  deliveries into `TriggerEvent` envelopes, appends accepted
  payloads onto `orchestrator.triggers.pending`, and drains
  in-flight requests during shutdown.
- **MVP auth middleware for orchestrator `a2a-push` routes (#180).**
  `harn orchestrator serve` now requires `Authorization` on manifest
  `a2a-push` endpoints while leaving webhook routes on their existing
  connector-level signature checks and keeping `/healthz` + `/readyz`
  public. Bearer auth accepts comma-separated API keys from
  `HARN_ORCHESTRATOR_API_KEYS`; HMAC auth accepts
  `Authorization: HMAC-SHA256 timestamp=<unix>,signature=<base64>`
  signed over `METHOD\nPATH\nTIMESTAMP\nSHA256(BODY)` with the shared
  secret from `HARN_ORCHESTRATOR_HMAC_SECRET`. Invalid or missing auth
  now returns `401 Unauthorized`, and the new listener coverage includes
  subprocess + conformance checks for unauthenticated, bad-HMAC, and
  valid-bearer requests.
- **SIGHUP-driven orchestrator manifest hot reload with versioned HTTP
  trigger swaps (#187).** `harn orchestrator serve` now handles Unix
  `SIGHUP` by reparsing `harn.toml`, reconciling manifest trigger
  bindings through the trigger registry, swapping `webhook` /
  `a2a-push` listener routes in place, and preserving the binding
  version that each in-flight request started with while new requests
  move to the new version. Successful and failed reloads are recorded
  on `orchestrator.manifest`, `orchestrator-state.json` is refreshed
  after successful reloads, and the trigger registry now garbage
  collects old terminated versions after a small retention window so
  repeated reloads do not leak stale bindings.
- **DST-safe cron connector with durable tick state and catch-up modes
  (#210, closes #169).** `harn_vm::connectors::CronConnector` now schedules
  named IANA time zones through `croner` + `chrono-tz`, persists the
  latest scheduled boundary per trigger on the `connectors.cron.state`
  EventLog topic, and supports `catchup_mode = "skip" | "all" |
  "latest"` when an orchestrator resumes after downtime. The scheduler
  fires repeated fall-back hours once, skips missing spring-forward
  hours instead of inventing wall-clock times, writes normalized cron
  `TriggerEvent` envelopes to `connectors.cron.tick`, and ships with
  docs at `docs/src/connectors/cron.md`.
- **Action-graph observability extended with `trigger` and `predicate`
  node kinds (#202, partial #163).** Persisted run records now
  synthesize a `trigger` node from `trigger_event` metadata, render
  workflow `condition` stages as `predicate` nodes, propagate
  `trace_id` across the derived action graph, and stream updates onto
  the shared `observability.action_graph` event-log topic.
  Dispatch/A2A/worker/DLQ nodes deferred to the T-06 dispatcher
  milestone.
- **Connector trait + registry + shared HMAC-signature verification
  (#203, closes #167).** New `harn_vm::connectors` module defines
  the async `Connector` + `ConnectorClient` traits,
  `ConnectorRegistry` with activation fan-out, a provider-scoped
  token-bucket rate limiter, and a shared `verify_hmac_signed(...)`
  helper covering GitHub-style, Stripe-style, and Standard Webhooks
  HMAC conventions. The HMAC helper operates on raw request-body
  bytes, uses constant-time comparison, enforces timestamp-window
  limits, and routes signature-verify failures through the
  `audit.signature_verify` EventLog topic. Ships with authoring
  docs at `docs/src/connectors/authoring.md`. Foundation for
  upcoming MVP connectors (cron, webhooks, GitHub, Slack, Linear,
  Notion, A2A push).
- **Generic webhook receiver connector (#168).** `harn_vm::connectors`
  now ships a built-in `GenericWebhookConnector` for inbound HTTP
  webhook deliveries. The connector verifies Standard Webhooks,
  Stripe-style, and GitHub-style HMAC signatures through the shared
  C-01 helper, normalizes verified payloads into `TriggerEvent`
  using `GenericWebhookPayload`, records verification failures on
  `audit.signature_verify`, exposes the built-in provider through
  `ConnectorRegistry::list()`, and adds authoring docs at
  `docs/src/connectors/webhook.md`. Listener routing/TLS integration
  remains deferred to O-02, and durable inbox-backed dedupe remains
  deferred to T-09.
- **Secret-provider primitives for reactive runtime work (#194,
  closes #154).** `harn_vm::secrets` now provides `SecretProvider`,
  `ChainSecretProvider`, zeroizing `SecretBytes`, and concrete env +
  keyring providers. MCP OAuth token storage now routes through the
  shared keyring provider, and `harn doctor` reports the active
  secret-provider chain plus per-provider health for env/keyring
  setups. Foundation for upcoming connector + orchestrator work.
- **Generalized `EventLog` primitive (#195, closes #153).** New
  `harn_vm::event_log` module provides a reusable append-only event
  log with pluggable backends (Memory, File/JSONL, SQLite) — the
  substrate for durable trigger state, connector inbox/outbox
  dedupe, and the orchestrator's event-sourced core. Existing
  session transcript + per-session agent-event sinks migrate onto
  the shared abstraction, `harn doctor` surfaces the active backend
  and on-disk footprint, and SQLite is the default when persisting.
- **`TriggerEvent` schema and `std/triggers` stdlib types (#196,
  closes #155).** New `harn_vm::triggers::event` module defines the
  typed `TriggerEvent` envelope with a provider-payload union and
  signature-status field, plus the `std/triggers` stdlib type
  surface scripts use to construct and inspect trigger events.
  Lays the type foundation that upcoming EventLog-backed trigger
  registry, LLM predicate gate, and handler dispatcher work will
  consume.
- **Daemon lifecycle events flow into run observability (#197,
  closes #143 part (c)).** Persisted runs derive `daemon_events`
  from daemon stdlib wrapper activity, `harn runs inspect` prints
  the lifecycle timeline, and the portal run detail view exposes a
  dedicated Daemons section alongside the rest of observability.
- **Re-triggerable workers via `carry_policy.retriggerable` (#198,
  closes #143 part (b)).** Workers can now park in an `awaiting`
  state after a successful run, wake back up through the new
  `worker_trigger(...)` builtin, and keep appending follow-up turns
  onto the same transcript instead of starting from scratch. One-
  shot workers remain the default, `worker_wait` now blocks
  retriggerable workers until a real terminal state, and the
  persisted worker snapshot keeps the new lifecycle/carry-policy
  fields across resume.
- **`[[triggers]]` manifest overlay parsing and validation (#199,
  closes #156).** `harn.toml` can now declare `[[triggers]]` entries
  alongside `[[hooks]]`, with typed parsing for trigger kinds
  (`cron`, `webhook`, `a2a-push`, `poll`, `stream`, `predicate`),
  retry/priority/budget config, and kind-specific fields. The loader
  reuses the manifest-extension ABI from #138 + #141 to resolve
  handler and `when` identifiers against exported Harn functions.
  Validation covers id uniqueness, handler URI schemes, cron
  schedules, JMESPath dedupe expressions, and secret-id namespaces.
  `harn doctor` now surfaces loaded triggers with id, kind, provider,
  handler kind, and budget. Ships with example manifests for
  github-new-issue, cron-daily-digest, and a2a-reviewer-fanout.
- **Hardened daemon stdlib queue semantics (#200, closes #157).**
  `daemon_trigger(...)` now pushes onto a bounded durable event
  queue (`daemon.meta.json` with atomic write-rename persistence)
  instead of pushing trigger payloads through the bridge as
  ephemeral user messages. Explicit `VmError::DaemonQueueFull` on
  overflow, idle-boundary-gated delivery so triggers only fire at
  turn boundaries, in-flight event re-queue across `daemon_stop` /
  `daemon_resume` for at-least-once delivery, queue metadata in
  snapshot + daemon summary, `docs/src/stdlib/daemon.md` +
  quickref coverage. Daemon lifecycle events (Triggered, Snapshotted,
  Stopped, Resumed) continue to flow into run observability at
  enqueue/snapshot/stop/resume boundaries.
- **Runtime-owned `TriggerRegistry` with lifecycle + versioning (#205,
  closes #158).** Thread-local registry in `harn_vm::triggers::registry`
  tracks per-binding state (`active`, `draining`, `removed`),
  per-binding metrics, and in-flight event counts. Every lifecycle
  transition logs to the shared `triggers.lifecycle` EventLog topic.
  `run`, `bench`, ACP, playground, and test execution paths now install
  manifest triggers into this live registry rather than only validating
  them. `harn doctor` surfaces the live binding view including
  provider, state, version, and metrics snapshot. Foundation for
  hot-reload (T-13) and the handler dispatcher (T-06).
- **Trigger stdlib wrappers for registry inspection and manual dispatch
  (#164).** Added `trigger_list`, `trigger_register`, `trigger_fire`,
  `trigger_replay`, and `trigger_inspect_dlq` as first-class builtins,
  plus typed `std/triggers` aliases for `TriggerBinding`,
  `TriggerConfig`, `DispatchHandle`, and `DlqEntry`. Scripts can now
  hot-install local triggers, inspect live binding metrics, fire
  synthetic events, perform shallow event-log replay, and inspect DLQ
  retry history in-process. `trigger_replay` now routes through the
  full dispatcher (see the #166 entry above); manual `worker://`
  dispatch remains deferred to O-05.

## v0.7.22

### Added

- **Declarative runtime hooks via `[[hooks]]` manifests (#146, closes
  #141, builds on #138).** `harn.toml` can now register process-scoped
  `PreToolUse`, `PostToolUse`, `PreAgentTurn`, `PostAgentTurn`, and
  worker lifecycle hooks from exported Harn functions before execution
  starts. Tool hooks support manifest-driven deny/argument-rewrite/
  result-rewrite behavior, worker lifecycle moved off raw status
  strings onto a typed `WorkerEvent` enum, and conformance now covers
  manifest registration, pre-tool short-circuit, and post-tool rewrite
  behavior end-to-end.
- **Daemon stdlib wrapper builtins for runtime-owned daemon mode
  (#145, closes #143 part (a)).** `daemon_spawn`, `daemon_trigger`,
  `daemon_snapshot`, `daemon_stop`, and `daemon_resume` now expose the
  existing agent-loop daemon runtime through a first-class stdlib
  handle. The wrapper stores daemon metadata alongside the persisted
  runtime snapshot so resumable state dirs can be reopened
  ergonomically without changing daemon semantics.
- **`transcript_compact(...)` now wraps the runtime-owned transcript
  compaction engine (#147, closes #142 part (a)).** The manual
  transcript compaction surface now reuses `AutoCompactConfig` with
  `llm`, `truncate`, and `observation_mask` strategies, supports
  prompt-template overrides for LLM summaries, preserves pre-
  compaction transcripts as durable embedded artifacts, and exposes
  compaction events through both transcript observability and the
  live `agent_subscribe` stream.
- **First-class `<user_response>` agent-protocol tag (#148).**
  Assistant responses can now emit a structured `<user_response>`
  block that the runtime surfaces separately from internal
  `<assistant_prose>`. Parsing, visible-text sanitization, and
  persistent-loop completion all honor the tag; the existing
  `<assistant_prose>` + `##DONE##` sentinel remains the fallback for
  uninstrumented prompts.
- **LLM-based Burin Mini semantic evaluator (#148).** New
  `experiments/burin-mini/evaluator.harn` grades an actual run
  against the ideal trace using the full run record plus inference
  transcript bundles. Four integration tests are `#[ignore]`'d while
  the Linux-CI timing issue is being replaced by the v2 experiment.

### Changed

- **Anthropic provider now caches at the request envelope (#148).**
  `cache_control` moves from per-system-block to the top-level
  request envelope (Anthropic's "automatic prompt caching" mode),
  which caches the stable tools + system + messages prefix across
  multi-turn loops. No semantic change to generations; just cheaper.
- **Sub-agent + session lineage is now append-only and parent-
  aware (#148).** Child sessions inherit parent context on spawn,
  parent sessions record `sub_agent_start` / `sub_agent_result`
  events against their own event lists, and resumed persistent
  loops restore the prompt surface correctly. Reuses #133's
  `WorkerProvenanceRecord` shape unchanged.
- **Workflow prompt contract tightened (#148).** Current stage is
  authoritative; `<workflow_context>` is now supporting evidence
  rather than additional instructions. Execute batches default to
  stage-local context instead of the full action-graph plan, and
  action-graph batch tool exposure is narrowed to what the current
  batch actually needs. Pipeline consumers that relied on cross-
  stage prompt leakage should audit their stage prompts.
- **Burin Mini replatform onto Harn workflow sessions (#148).** The
  experiment now uses Harn-native workflow stages, shared workspace
  helpers, profile-driven planning, and transcript-backed artifacts
  instead of the removed Rust host capability layer.
- **Burin Mini live-eval planner/batching stabilization (#193).**
  Planner normalization now folds verify actions into a single
  run-only verify batch instead of leaking them into execute/write
  batches, tolerates recoverable planner JSON nulls, and forces a
  final planner commit pass once the research budget is exhausted.
  Research/planner prompts disambiguate local composition vs.
  architecture redesign, and speculative research-worker advice is
  dropped. Two transcript-derived `#[ignore]`'d regressions lock the
  weak-verify-plan and over-researching-planner behaviors. Validated
  across three back-to-back live runs with local
  `qwen3.5:35b-a3b-coding-nvfp4`.
- **`approval_policy.write_path_allowlist` no longer blocks read-
  only tools (#193).** The allowlist now gates only write-class
  tools (`edit`, `write`, `delete`, `move`); `read`/`look`/`search`/
  `run` traffic is unaffected. Action batches also auto-inject the
  allowlist from declared target paths so downstream pipelines don't
  have to wire it by hand — downstream consumers should audit any
  place they relied on the old (stricter) gate.
- **`ledger` tool now fails fast when no task ledger is active
  (#193).** Previously returned a silent empty result; now surfaces
  a typed error so pipelines learn about the missing context
  instead of silently producing empty plans.

### Fixed

- **Tree-sitter grammar recognizes backslash line continuation
  (#149, closes #144).** The grammar's `extras` rule now treats
  `\\\n` (optionally followed by indentation) as ignorable
  whitespace, matching lexer/runtime behavior. Fixes the v0.7.21
  release-audit blocker on
  `conformance/tests/agents/workflow_subagent_runtime.harn` and
  unblocks future `\` continuation usage in conformance fixtures.

## v0.7.21

### Added

- **Manifest-backed runtime extension ABI (#128).** `harn.toml` now
  supports `[exports]` for stable package module entry points and
  `[llm]` for packaged provider definitions, aliases, inference rules,
  and model defaults. Runtime imports and the static module graph both
  resolve package exports and search the nearest ancestor
  `.harn/packages/` root, so packages can ship capability modules and
  provider adapters without Rust-side registration edits.
- **`project_enrich` L2 enrichment primitive (#110, closes #102).** New
  native-backed stdlib fn that layers a caller-owned LLM enrichment
  pass on top of deterministic `project_scan` evidence. Caller supplies
  the prompt template + output schema; Harn owns prompt rendering,
  bounded file selection, schema-retry plumbing, and content-hash
  caching under `.harn/cache/enrichment/`. Budget-token short-circuit
  returns the base evidence with `budget_exceeded: true` instead of
  failing. Schema-retry exhaustion returns `validation_error` +
  `base_evidence` instead of raising.
- **`project_deep_scan` cached per-directory tree (#111, closes #103).**
  Namespace-scoped hierarchical cache built on top of the metadata
  store. Reuses cached directory-level structure + content hashes
  across recursive walks. `project_deep_scan_status(namespace, path?)`
  surfaces the latest run summary (`total_dirs`, `enriched_dirs`,
  `stale_dirs`, `cache_hits`, `last_refresh`). Metadata shards persist
  under `.harn/metadata/<namespace>/entries.json` while legacy
  root-metadata reads remain backward-compatible. `harn doctor`
  surfaces metadata cache state.
- **First-class action-graph planning helpers (#134, closes #123).**
  `std/agents` now exposes `action_graph(...)`,
  `action_graph_batches(...)`, `action_graph_render(...)`,
  `action_graph_flow(...)`, and `action_graph_run(...)` on top of the
  existing workflow runtime. Planner output variants normalize into a
  canonical action-graph envelope, missing research->execute /
  execute->verify dependencies are repaired conservatively, ready work
  batches by phase and tool class, and shared terminal verify/evaluate
  stages can be composed without hand-wiring the workflow graph in
  every pipeline.
- **Worker request/provenance retention for delegated/background agents
  (#133, closes #124).** Worker handles, waited results, snapshots,
  child-run records, and `worker_result` artifacts now preserve
  immutable original `request` metadata plus normalized `provenance`
  fields. `std/agents` adds `worker_request`, `worker_result`,
  `worker_provenance`, `worker_research_questions`,
  `worker_action_items`, `worker_workflow_stages`, and
  `worker_verification_steps` helpers so parent orchestration can
  recover structured child metadata without index-based rebinding.
- **`harn playground` + Burin Mini experiment checkpoint (#129).**
  Fixture and recording coverage for the `harn playground` subcommand
  plus a committed `experiments/burin-mini/` scaffold with a tiny
  auth-demo workspace, deterministic fixtures, a live-suite runner,
  and transcript-backed analysis notes. Tightens native-tool / Ollama
  integration so local Qwen-class models can use structured tool
  calls and JSON-mode responses reliably, and enforces native
  action-loop behavior on tool-gated stages.
- **Transcript-synthesized JSON results for `sub_agent_run(...)`
  (#132, closes #122).** Parent sub-agent summaries and structured
  `data` now derive from assistant transcript history, so JSON-mode
  child runs are not lost when the final visible text is empty,
  sentinel-only, or otherwise not parseable. `returns.schema`
  validation remains anchored to the recovered transcript JSON.
- **Verifier contracts as first-class workflow inputs (#135, closes
  #126).** Workflow verifier metadata is normalized into structured
  verification contracts that can carry exact identifiers, paths,
  required text, and optional sidecar JSON contract files. Those
  contracts are injected into stage prompts and run metadata
  automatically so planning/execution stages see verifier-exact
  requirements before editing, rather than having to rediscover them
  from ad hoc prompts.
- **Workspace path normalization across tool boundaries (#136, closes
  #125).** New shared workspace-path classifier distinguishes
  `workspace_relative`, `host_absolute`, and `invalid` paths.
  Declared tool path arguments are normalized centrally before
  dispatch so common leading-slash drift like `/packages/...` is
  recovered to workspace-relative form when it safely maps into the
  current workspace. New public `path_workspace_info(...)` and
  `path_workspace_normalize(...)` builtins plus `std/path` wrappers
  surface the classifier to scripts; declared-path metadata is
  exposed to approval/permission flows while existing string
  summaries are preserved.
- **Action-graph observability on run records (#137).** Persisted run
  records now carry a derived `observability` block that bundles
  planner rounds, research facts, action-graph structure, worker
  lineage, verification outcomes, and transcript pointers into a
  single artifact. `harn runs inspect`, portal run detail, and portal
  compare all surface it so regressions show up beyond coarse stage /
  status drift.
- **Manifest-backed runtime extension ABI (#138, closes #128).** New
  `[exports]` package entry points let modules publish stable import
  surfaces without core runtime edits, and new `[llm]` manifest
  overlays let packages and projects register provider aliases,
  inference rules, tiers, and model defaults declaratively. Runtime
  and static import resolution consult ancestor `.harn/packages`
  roots plus package export maps while preserving existing `lib.harn`
  fallback behavior. Package and root manifest overlays are loaded at
  runtime so approval policy, transcripts, replay, and eval tooling
  continue to execute through the existing runtime trust boundary.

### Changed

- **`project_deep_scan` enriched tier now reuses `project_enrich_native`
  (#115).** The duplicate Harn-level deep-scan enrichment wrapper is
  gone; deep-scan enriched refreshes share the native cache, budget
  gate, schema-retry semantics, and option plumbing (including
  `temperature`) with `project_enrich`. Namespace-scoped native cache
  keys preserve per-namespace invalidation.
- **`sub_agent_run(...)` honors workflow-level skill context (#118,
  closes #116).** When a sub-agent call does not specify its own
  `skills:` / `skill_match:` options, the workflow-level skill
  context installed by `workflow_execute(...)` is now inherited.
  Explicit per-call options keep higher priority; child tool schemas
  narrow to the workflow-scoped read namespace as expected.
- **Split `crates/harn-vm/src/llm/helpers/mod.rs` (#112, closes #60)**
  into topic-focused submodules (`blocks`, `messages`, `opt_get`,
  `provider`, `transcript`). `mod.rs` shrinks from 1266 lines to a 20-
  line re-export hub. Pure refactor; behavior unchanged.
- **Split `crates/harn-vm/src/stdlib/agents_workers.rs` (#113, closes #56)**
  into `audit`, `bridge`, `config`, `execution`, `policy`, `tests`,
  `worktree` submodules plus an extracted `agents_sub_agent.rs`. Pure
  refactor; behavior unchanged.
- **Split `crates/harn-vm/src/schema.rs` into `schema/` module tree
  (#117).** Focused files for API entrypoints, validation, transforms,
  type helpers, canonicalization/export, and result helpers; VM-facing
  `crate::schema::*` entrypoints and `json_to_vm_value` remain intact.
  Pure refactor; behavior unchanged.
- **Split `crates/harn-fmt/src/formatter.rs` into `formatter/` modules
  (#119, closes #53).** Core state, comments, declarations,
  expressions, and statement/block helpers now live in focused
  modules; the public `format_source` API stays in `lib.rs`. Pure
  refactor; formatter behavior unchanged.
- **Split `crates/harn-parser/src/builtin_signatures.rs` into focused
  namespace-oriented groups (#120, closes #48).** Central
  `all_signatures()` concatenates the group slices and keeps the
  parser/runtime registry alignment guard. Pure refactor.
- **Split `crates/harn-vm/src/stdlib/template.rs` into a `template/`
  module tree (#121, closes #46).**
  `crate::stdlib::template::render_template_result` remains the
  single script/host entrypoint, preserving the single-source-of-
  truth contract called out in CLAUDE.md. Pure refactor.
- **Split `crates/harn-vm/src/vm/methods.rs` by receiver type (#130,
  closes #55).** `Vm::call_method` stays the single entrypoint in
  `dispatch.rs`; receiver-specific handlers now live in focused
  modules for strings, lists, dicts, sets, ranges, iterators,
  generators, struct instances, and number dispatch. Pure refactor.
- **Split `crates/harn-vm/src/llm/tools/tests.rs` (#131, subsumed by
  #140).** Initial subject-focused split of the `llm::tools` test
  file into per-concern modules; the final file layout in-tree is
  the one from #140 below.
- **Refactor `crates/harn-vm/src/vm.rs` into smaller VM modules (#139,
  closes #47).** The monolithic `vm.rs` splits into focused VM
  modules with a minimal `vm/mod.rs`; import-loading code moves into
  `vm/modules.rs`. The inline VM test module splits into dedicated
  debug and runtime test modules. Public VM surface unchanged.
- **Split `crates/harn-vm/src/llm/tools/mod.rs` into focused modules
  (#140, closes #61).** Message shaping, schema collection, prompt
  rendering, native tool conversion, and type/schema helpers live in
  their own modules; oversized parser and test files split into
  submodules so every file in the `llm/tools` area stays under the
  target size. The public `tools` module stays a thin re-export hub.

### Fixed

- **Playground env-mutating tests now serialize.** `ScopedEnv::apply`
  writes process-wide env vars; running the three playground tests
  that exercise it concurrently under `cargo test` intermittently
  tripped "Missing API key" failures once enough tests landed on
  main. The affected tests now serialize on a shared
  `tokio::sync::Mutex` (`playground_env_lock()`) so the env overlay
  is seen consistently across the await points the tests hit.
- **Post-merge compile fixes.** `LlmMock` literals in
  `stdlib/workflow/tests.rs` now include the `consume_on_match`
  field introduced by #132 (the struct literal from #135 missed it),
  and `llm/helpers/transcript.rs` strips a trailing blank line that
  failed `cargo fmt --check` after dead-code cleanup.

## v0.7.20

### Added

- **`harn playground` CLI (#109, closes #99).** New `harn playground`
  subcommand runs a Harn script against an in-process Harn host for
  fast pipeline iteration without JSON-RPC bridge boilerplate. Flags:
  `--host <file>` (exports host functions), `--script <file>` (the
  pipeline under test), `--task <string>` (forwarded to the script's
  `task` parameter), `--llm mock:<fixtures>` (pairs with the new
  `--llm-mock` replay infra), and `--watch` (re-runs on edit).
  Missing host-capability failures now report the missing function
  name with caller context instead of a generic bridge error.
  Companion `pipeline-lab` scaffolding ships in
  `crates/harn-cli/src/commands/init.rs`. Intended as the substrate
  for prototyping multi-agent architectures end-to-end without the
  crates.io release cycle.

- **`load_skill(name)` runtime tool + always-on catalog (#108, closes
  #96).** `agent_loop` configured with a skills registry now exposes a
  first-class `load_skill(name)` tool the agent can call mid-session
  to promote a deferred skill body into the active prompt. Two helper
  builtins — `skills_catalog_entries` and
  `render_always_on_catalog` — render the compact catalog harnesses
  advertise in the always-on prompt. `disable-model-invocation` and
  `allowed-tools` flow through both the VM-side text channel and the
  native-channel tool narrowing so a loaded skill's tool surface
  matches its frontmatter.

- **`std/project` scan builtins (#105, closes #97).** New deterministic
  L0/L1 project-evidence primitives for non-LLM dispatch:
  `project_scan(path, options)` returns a single directory's evidence
  (languages, frameworks, build systems, confidence); `project_scan_tree`
  walks recursively for polyglot repos and returns a per-directory
  dict keyed by relative path; `project_catalog()` exposes the
  detector catalog itself so callers can extend detection by shipping
  entries rather than patching Rust. `.gitignore`, vendor-dir skipping,
  and shared package-name parsing now share one deterministic path
  that also feeds `project_root_package()`.

- **`sub_agent_run(task, options)` context-firewall primitive (#107,
  closes #98).** New VM builtin that runs a nested agent loop in an
  isolated child session and returns a typed envelope (`summary`,
  `artifacts`, `evidence_added`, `tokens_used`, `budget_exceeded`,
  `session_id`, `ok`, `error`, optional `data`). The child's
  transcript stays in the child session, so the parent transcript
  records only the outer call/result pair. `allowed_tools` narrows
  the child's tool surface via intersection with inherited policy;
  `returns: { schema: ... }` produces a structured result envelope;
  `background: true` returns a worker handle compatible with the
  existing `wait_agent` / `list_agents` / `resume_agent` lifecycle
  builtins. Child session lineage is recorded in the session store.

- **`std/agent_state` durable session state (#106, closes #101).** New
  module that persists small durable blobs under a caller-owned root
  keyed by session id, with atomic writes, resumable handles, and a
  reserved well-known key for structured handoff documents. The
  backend is a stable trait with a filesystem implementation so
  future backends (e.g. a real KV store, a host-managed sandbox) can
  plug in without changing the Harn-facing API. Covers round-trip,
  cross-process resume, and two-writer conflict behavior via
  conformance tests. Substrate for the later `project.deep_scan()` L3
  cache (harn#103).

- **`harn run --llm-mock` / `--llm-mock-record` (#104, closes #100).**
  Surfaces the existing VM-side mock infrastructure as first-class
  `harn run` flags: `--llm-mock <fixtures.jsonl>` replays LLM
  responses from a JSONL fixture file (FIFO by default, glob match
  via `"match"` field), `--llm-mock-record <fixtures.jsonl>` captures
  real provider responses into a fixture file. Unmatched prompts fail
  with a snippet of the prompt that didn't match. Intercepts
  non-`mock` providers in replay mode so fixture replay never hits
  live APIs. Pairs with `harn playground --llm mock:<fixtures>` for
  deterministic pipeline iteration.

- **Agent event variants for `tool_search_query` / `tool_search_result`
  (harn-vm, harn-cli).** Both the client-executed fallback
  (`crates/harn-vm/src/llm/agent/tool_search_client.rs`) and the
  provider-native paths (Anthropic / OpenAI Responses server-hosted
  tool search) now emit canonical `AgentEvent::ToolSearchQuery` and
  `AgentEvent::ToolSearchResult` alongside the existing transcript
  events. `AcpAgentEventSink` forwards both as `session/update`
  notifications with `sessionUpdate: "tool_search_query"` /
  `"tool_search_result"` kinds so ACP hosts (burin-code et al.) can
  render a "Tool Vault search in progress" chip in real time.
  Previously these events existed only as content blocks on the
  assistant's response and transcript-events list, so IDEs could not
  observe a search until the whole turn completed. `mode` is tagged
  `"client"`, `"anthropic"`, or `"openai"` so downstream consumers
  can distinguish the path. `AgentEvent` is not `#[non_exhaustive]`
  so this is a SemVer-breaking change for out-of-tree consumers that
  match the enum exhaustively — add arms for the two new variants.

- **`skills:` / `skill_match:` pass-through in `workflow_execute`
  (harn-vm).** `workflow_execute(task, graph, artifacts, {skills:
  ..., skill_match: ...})` now threads the registry through every
  per-stage agent loop via a workflow-level thread-local context
  (`WorkflowSkillContext`). Per-node `model_policy.skills` /
  `model_policy.skill_match` overrides the workflow-level setting,
  mirroring the precedence that already holds for other model_policy
  fields. Before this, only direct `agent_loop(...)` callers received
  the `skills:` option — workflow-graph callers silently dropped it,
  which was blocking burin-code's Skills & Tool Vault cutover.

- **Namespace-prefixed entries in skill `allowed_tools` (harn-vm).**
  A skill's `allowed_tools` list now accepts three shapes per entry:
  an exact tool name (unchanged), `"namespace:<tag>"` to match every
  tool declared with that `namespace` field, and `"*"` as a "keep the
  full surface" escape hatch useful for skills that only want to
  carry prompt context without narrowing the tool surface. Namespace
  matching lands in both the VM-side `skill_scoped_tools_val` (text
  channel + contract prompt) and the native-channel
  `rebuild_scoped_native_tools` (OpenAI Responses / Anthropic JSON
  schema lists). Malformed entries — `"namespace:"` with no tag, or
  any other colon-prefixed token — fail loud at `skill_define` time
  rather than silently scoping to an empty set.

- **`model_policy.tool_format` on workflow nodes (harn-vm).** The
  per-stage agent loop previously resolved its tool-calling contract
  format solely from `HARN_AGENT_TOOL_FORMAT` env / provider-model
  default. `ModelPolicy` gained an optional `tool_format: Option<String>`
  field that takes precedence, so workflow authors can pin
  `tool_format: "native"` per-stage without touching env or rebuilding
  the pipeline runner.

### Fixed

- **Module graph path-spelling explosion (harn-modules).**
  `harn_modules::build()` deduped discovered imports on the raw path
  returned by `resolve_import_path`, which preserves `..` segments
  (`base.join(import)` without collapsing). Two files in sibling
  directories that imported each other (e.g. `lib/context/a.harn`
  importing `../runtime/b.harn` and vice versa) produced a fresh path
  spelling on every round-trip — `.../context/../runtime/`,
  `.../context/../runtime/../context/`, `.../context/../runtime/../context/../runtime/`,
  and so on. Because each spelling was treated as a new module, the
  walk only terminated when `path.exists()` started failing at the
  filesystem's `PATH_MAX`. macOS's effective `PATH_MAX` of 1024
  masked the blow-up; Linux's `PATH_MAX` of 4096 let the walk run
  ~4× longer, re-parsing the same pair tens of thousands of times —
  RSS ballooned to 7+ GB and GitHub Actions runners SIGTERM'd or
  SIGKILL'd the process. Symptom was a `harn lint <dir>` or
  `harn check --workspace` that looked like a hang at 0% CPU
  (actually thrashing and eventually OOM-killed). `build()` now
  canonicalizes each import path through `normalize_path` before
  inserting into the `seen` set, so the graph size is bounded by the
  number of underlying files rather than path-spelling cycles. On a
  representative 88-file pipeline tree, Linux lint dropped from
  OOM-killed-at-48s (7.7 GB RSS) to 0.22s / 16 MB; macOS dropped
  from 6.7s / 1.2 GB to 0.09s / 14 MB.

## v0.7.19

### Fixed

- **Release workflow cross-compile target (release.yml).** After
  `rust-toolchain.toml` pinned the repo to Rust 1.95.0,
  `dtolnay/rust-toolchain`'s `targets:` input was installing the
  requested target against the `stable` channel only, while cargo then
  picked up the pinned 1.95.0 toolchain (without rust-std for the
  matrix target) and failed with `E0463: can't find crate for core`.
  The release build now runs an explicit `rustup show` + `rustup target
  add ${{ matrix.target }}` so the matrix target is installed against
  the active (pinned) toolchain. No change required to
  `rust-toolchain.toml` when bumping the pin.

- **mcp_card test flakiness (harn-vm).** Three tests in
  `mcp_card::tests` were each calling `reset_cache()` on the
  process-wide `CARD_CACHE`, which races under default parallel test
  execution and could wipe the cached entry mid-assertion — producing
  intermittent `Some("updated") != Some("cached")` failures on CI. The
  two callers that don't actually touch the cache dropped the
  defensive reset; the TTL test now holds a static serialization mutex
  so future cache-touching tests take their turn.

## v0.7.18

### Added

- **Skills CLI + portal observability (harn#76).** `harn skills` now
  ships five subcommands for managing and inspecting the layered skill
  catalog without running a pipeline:

  - `harn skills list` — shows every resolved skill in priority order
    with the layer it came from; `--all` includes shadowed entries,
    `--json` emits newline-delimited JSON for piping.
  - `harn skills inspect <name>` — dumps frontmatter, bundled files,
    and the full SKILL.md body for one skill. Accepts bare `<name>`
    or fully-qualified `<namespace>/<name>`.
  - `harn skills match "<query>"` — runs the agent-loop metadata
    matcher against a prompt and prints ranked candidates with their
    scores + reasons. Useful when tuning a SKILL.md's `description:` /
    `when_to_use:` frontmatter.
  - `harn skills install <spec>` — materializes a git URL, `owner/repo`
    shorthand, or local path into `.harn/skills-cache/` so the
    filesystem package walker picks it up on the next run. Supports
    `--tag`, `--namespace`, and rewrites `.harn/skills-cache/skills.lock`.
  - `harn skills new <name>` — scaffolds a SKILL.md + `files/` bundle
    under `.harn/skills/<name>/` with sensible frontmatter defaults.

  The portal's run detail page gains three observability panels
  derived from the persisted transcript events:

  - **Skill timeline** — horizontal bars showing which skills
    activated on which agent-loop iteration and when they
    deactivated, with matcher score and reason on hover.
  - **Tool-load waterfall** — one row per `tool_search_query`
    transcript event, paired with its `tool_search_result` so you can
    see which deferred tools entered the LLM's context in each turn.
  - **Matcher decisions** — per-iteration expansions showing every
    candidate the matcher considered, with scores and working-file
    snapshots.

  The runs index page also accepts a `skill=<name>` filter (both via
  the URL and a new input on the runs page) for selecting evals where
  a specific skill was active. `docs/src/skills.md` gains a
  "Managing skills" section covering the new commands.

- **Tool Vault phase 4: data-driven provider capabilities (harn#77).**
  The per-provider / per-model capability gates used by the tool-search
  and defer-loading paths (hard-coded Rust `match` blocks added in
  harn#69 and harn#71) are now a data table. A shipped
  `crates/harn-vm/src/llm/capabilities.toml` declares one rule per
  family:

  ```toml
  [[provider.anthropic]]
  model_match = "claude-opus-*"
  version_min = [4, 0]
  native_tools = true
  defer_loading = true
  tool_search = ["bm25", "regex"]
  max_tools = 10000
  prompt_caching = true
  thinking = true

  [[provider.openai]]
  model_match = "gpt-*"
  version_min = [5, 4]
  native_tools = true
  defer_loading = true
  tool_search = ["hosted", "client"]
  ```

  - Matcher is glob + semver: `model_match` is a `*`-glob against the
    lowercased model ID, `version_min` is a `[major, minor]` lower
    bound parsed with the same Claude / GPT version extractors the
    providers used before.
  - `[provider_family]` declares sibling providers that inherit rules
    from a canonical family. OpenRouter, Together, Groq, DeepSeek,
    Fireworks, HuggingFace, and local vLLM all fall through to
    `[[provider.openai]]` by default.
  - New `[[capabilities.provider.<name>]]` section in `harn.toml` lets
    users override or extend the matrix per-project. Useful for
    flagging a proxied OpenAI-compat endpoint as supporting
    `tool_search` ahead of a Harn release. User rules take precedence
    over built-in rules for the same provider name.
  - `provider_capabilities(provider, model)` stdlib builtin returns a
    dict (`native_tools`, `defer_loading`, `tool_search`, `max_tools`,
    `prompt_caching`, `thinking`) so scripts can branch on the
    capability surface without vendor-specific knowledge:

    ```harn
    let caps = provider_capabilities("anthropic", "claude-opus-4-7")
    if "bm25" in caps.tool_search { ... }
    ```

    `provider_capabilities_install(toml_src)` and
    `provider_capabilities_clear()` expose the override path in-script
    for conformance tests and for scripts that detect proxied
    endpoints at runtime.
  - `LlmProvider::supports_defer_loading` and
    `native_tool_search_variants` now default-delegate to
    `capabilities::lookup` — the Anthropic and OpenAI provider impls
    no longer carry their own gate logic, so a new model generation
    needs one rule in the TOML instead of an `if` branch in Rust.
  - Conformance fixtures under `conformance/tests/provider_capabilities_*`
    cover the built-in matrix, the mock provider's dual-shape
    routing, and the user-override path (both adding a new provider
    and shadowing a built-in).

- **MCP Server Cards, lazy boot, skill-scoped binding (harn#75).**
  Harn now consumes MCP v2.1 Server Cards, defers booting MCP servers
  until a skill or user code actually needs them, and wires skill
  `requires_mcp` declarations into the agent loop's activation/deactivation
  hooks.
  - `harn.toml` `[[mcp]]` entries gain `lazy = true`, optional `card =
    "<url-or-path>"`, and `keep_alive_ms` for post-release grace. Lazy
    servers are registered with a process-wide registry but not booted
    until first use.
  - New builtins: `mcp_server_card(name|url|path)` (fetches + caches with
    a 5-minute TTL; falls back to `/.well-known/mcp-card` on bare HTTP
    URLs), `mcp_ensure_active(name)`, `mcp_release(name)`,
    `mcp_registry_status()`.
  - Skill activation ref-counts MCP server binders: `requires_mcp` (or
    legacy `mcp`) triggers `mcp_ensure_active` on every listed server;
    deactivation decrements. At count zero the server disconnects
    (immediately or after `keep_alive_ms`). Transcript events
    `skill_mcp_bound`, `skill_mcp_unbound`, `skill_mcp_bind_failed`
    ride along.
  - `mcp_list_tools` now stamps every returned tool with
    `_mcp_server: "<name>"`, and the client-side `tool_search`
    BM25 index auto-tags these tools as `mcp:<server>` and `<server>`
    so queries like `"github"` surface every tool from that server.
  - `harn mcp-serve` learns `--card <path-or-json>` which embeds the
    Server Card into the `initialize` response's `serverInfo.card`
    field and exposes it as the well-known resource
    `well-known://mcp-card`.
  - Conformance coverage: `mcp_server_card.harn`, `mcp_lazy_registry.harn`.
  - Docs: `docs/src/mcp-and-acp.md` gains sections on lazy boot, Server
    Cards, skill-scoped binding, and `--card`.
- **Skills & Tool Vault phase 3: `agent_loop` skill lifecycle (harn#74).**
  `agent_loop` now accepts a `skills:` option (a `skill_registry`
  produced by the `skill { }` top-level form or `skill_define(...)`)
  and runs a match-activate-reassess phase around every turn. The
  default metadata matcher scores skills by BM25-ish keyword overlap
  over `description` + `when_to_use`, name-in-prompt mentions, and
  `paths:` glob matching against the host-supplied `working_files:`
  list; opt into host-delegated ranking (embedding / LLM scorers /
  whatever) via `skill_match: { strategy: "host" }` or `"embedding"`
  — both route through a new `skill/match` JSON-RPC bridge method.
  - Activation binds the skill's `prompt` body into the effective
    system prompt, narrows the tool surface via its `allowed_tools`
    whitelist (union when multiple skills are active), and calls
    its `on_activate` hook. Deactivation (in `sticky: false` mode)
    unwinds everything and calls `on_deactivate`.
  - `disable-model-invocation: true` and `user-invocable: false`
    SKILL.md frontmatter are honoured: the matcher skips disabled
    skills entirely; `user-invocable` rides through for host UIs.
  - Transcript events `skill_matched`, `skill_activated`,
    `skill_deactivated`, `skill_scope_tools` emit with stable
    schemas. The first three also emit as `AgentEvent` variants so
    ACP hosts see live session updates (`harn-cli`'s ACP server
    translates them into `session/update` notifications).
  - Session-resume: when `session_id:` is set, the active skill set
    at the end of one run is persisted in the session store and
    rehydrated on the next `agent_loop` invocation, skipping
    iteration-0 match so sticky re-entry stays hot.
  - Conformance coverage under `conformance/tests/skill_lifecycle_*`.
- **Skills phase 2: filesystem `SKILL.md` loader + layered discovery (harn#73).**
  `harn run` / `harn test` / `harn check` now pre-populate the `skills`
  VM global with every `SKILL.md` they find across eight priority
  layers: `--skill-dir` (CLI), `$HARN_SKILLS_PATH` (env),
  `.harn/skills/` (project), `harn.toml` `[skills] paths` &
  `[[skill.source]]` (manifest), `~/.harn/skills` (user),
  `.harn/packages/**/skills/` (package), `/etc/harn/skills` &
  `$XDG_CONFIG_HOME/harn/skills` (system), bridge-registered (host).
  Frontmatter follows Anthropic / Claude-Code's Agent Skills spec
  (`name`, `description`, `when-to-use`, `disable-model-invocation`,
  `allowed-tools`, `user-invocable`, `paths`, `context`, `agent`,
  `hooks`, `model`, `effort`, `shell`, `argument-hint`); unknown
  fields surface as `harn doctor` warnings, not hard errors, so the
  spec can evolve without breaking older VMs. New `skill_render(skill,
  args)` builtin applies `$ARGUMENTS` / `$N` / `${HARN_SKILL_DIR}` /
  `${HARN_SESSION_ID}` substitutions to the SKILL.md body. Bridge
  protocol gains `skills/list` + `skills/fetch` requests and a
  `skills/update` notification for host-driven hot-reload. See
  `docs/src/skills.md` for the full reference and
  `docs/src/bridge-protocol.md` for the wire format.
- **Tool Vault phase 3: OpenAI Responses-API native `tool_search` (harn#71).**
  `tool_search` now flows through OpenAI's native progressive-disclosure
  mechanism on GPT 5.4+ with zero script changes: the capability gate
  detects the model generation (via `gpt_generation()` — parses
  `gpt-5.4-preview`, `gpt-5.4-turbo`, `gpt-5-4-20260115`, and
  OpenRouter-style `openai/gpt-5.4` prefixes), prepends the meta-tool
  `{"type": "tool_search", "mode": "hosted"}` to the tools array, and
  emits `defer_loading: true` on each deferred user tool's wrapper.
  Server-executed `tool_search_call` / `tool_search_output` entries in
  the response get parsed into the same `tool_search_query` /
  `tool_search_result` transcript events as the Anthropic path —
  replays are indistinguishable across providers. OpenRouter, Together,
  Groq, DeepSeek, Fireworks, HuggingFace, and `local` all inherit the
  same capability check; when their routed model ID matches `gpt-5.4+`
  they forward the payload unchanged.
- **`namespace: "<label>"` on `tool_define(...)`** groups deferred tools
  for OpenAI's `tool_search` meta-tool. Distinct namespaces are
  collected into the meta-tool's `namespaces` field (sorted, deduped).
  Anthropic ignores the label — harmless passthrough for replay
  fidelity. Type-validated: non-string values error at `tool_define`
  time so typos surface immediately.
- **Escape hatch `<provider>: {force_native_tool_search: true}`** on
  call options forces the hosted OpenAI path regardless of model
  detection. Useful for self-hosted routers and enterprise gateways
  whose model IDs Harn cannot parse but that forward `tool_search` +
  `defer_loading` unchanged.
- **Mock provider spoofs native capability by model generation.** When
  a conformance test writes `provider: "mock", model: "gpt-5.4"` or
  `"claude-sonnet-4-6"`, the capability gate reports native support so
  the test can exercise the real native payload shape via
  `llm_mock_calls()[0].tools`. Non-matching models still report no
  native support (used by `tool_search_unsupported_provider.harn`).
- **Response-parser coverage for OpenAI `tool_search_call` /
  `tool_search_output`.** Both non-streaming and SSE streaming paths
  now strip these blocks from the dispatchable `tool_calls` vector
  (they're server-executed) and record them as transcript events with
  the same shape Anthropic's `server_tool_use` /
  `tool_search_tool_result` emits. The empty-response sanity check
  exempts calls whose output consists entirely of these blocks.
- **New `crates/harn-vm/src/llm/providers/openai_compat.rs` helpers.**
  `gpt_generation(model)` parses major/minor from GPT model IDs;
  `gpt_model_supports_tool_search(model)` gates on `(major, minor) >=
  (5, 4)`. Unit-tested on dotted (`gpt-5.4`), dashed (`gpt-5-4`),
  namespaced (`openai/gpt-5.4-turbo`), and dated
  (`gpt-5-20260115` → `(5, 0)`, unsupported) forms.
- **Conformance tests.** `tool_search_native_openai.{harn,expected}`
  verifies the native injection + deferred-flag passthrough +
  unsupported-model diagnostic. `tool_search_namespace.{harn,expected}`
  verifies namespace passthrough through the registry, into the
  OpenAI wrapper, and into the meta-tool's `namespaces` field.
  `tool_search_provider_overrides.{harn,expected}` verifies the
  escape hatch.
- **Tool Vault phase 2: universal client-executed `tool_search` fallback (harn#70).**
  `tool_search` now works on every provider, not just the
  Anthropic-native path landed in phase 1. When the active provider
  lacks native `defer_loading` (Gemini, Ollama, OpenAI pre-5.4,
  Together, Fireworks, Groq, Deepseek, HuggingFace, local, mock),
  Harn auto-switches to an in-VM fallback: a synthetic
  `__harn_tool_search` tool is injected, the deferred tools are
  stripped from the initial turn's schema list, and when the model
  calls the synthetic tool the configured strategy runs against the
  deferred-tool corpus and the matching tools get promoted onto the
  *next* turn's schema list. The option surface is unchanged —
  `tool_search: "bm25"` / `"regex"` / `true` / `{variant, mode, ...}`
  all Just Work on any provider. `mode: "auto"` falls back silently;
  `mode: "client"` forces the fallback even on native-capable
  providers.
- **Four client-mode strategies.**
  - `"bm25"` (default) — tokenized BM25 over tool
    `name + description + parameter text`, matching Anthropic's native
    ergonomic for cross-provider consistency.
  - `"regex"` — case-insensitive Rust-regex over the same corpus
    (no backreferences / lookaround; see the regex crate docs).
  - `"semantic"` — delegates to the host via a new
    `tool_search/query` bridge RPC so integrators can wire embeddings
    without Harn depending on ML crates.
  - `"host"` — same RPC shape as semantic; the host decides how to
    rank. The VM round-trips the query and promotes whatever names
    come back.
- **New client-mode knobs on `tool_search`.** `budget_tokens: N`
  (soft cap with oldest-eviction for promoted schemas),
  `name: "find_tool"` (rename the synthetic search tool so skills can
  pick a verb the model prefers), `include_stub_listing: true`
  (append a short list of deferred-tool names to the contract prompt
  so the model can eyeball what's available without a search call),
  and `strategy: "..."` (explicit strategy override independent of
  `variant`, so you can pick a BM25-framed prompt with a semantic
  backend, for example).
- **`tool_search/query` bridge RPC.** Standard JSON-RPC request
  issued by the VM for `strategy: "semantic"` / `"host"`. Payload:
  `{strategy, query, candidates}`; response: `{tool_names, diagnostic?}`
  (or the ACP wrapper `{result: {...}}`). Documented in
  `docs/src/bridge-protocol.md`.
- **Cross-provider transcript parity.** Client-mode
  `tool_search_query` / `tool_search_result` events use the same
  shape as the Anthropic-native path — id, name, query / tool_use_id,
  tool_references — so replayers and analytics stay agnostic.
  Metadata adds `mode: "client"` tagging for distinguishing paths
  when that matters.
- **New `crates/harn-vm/src/llm/tool_search/` module.** In-tree BM25
  and regex indices with per-strategy tests. BM25 uses the conventional
  `k1 = 1.5`, `b = 0.75`; tokenization splits on non-alphanumeric
  boundaries so `open file` matches `open_file`.

### Changed

- **`tool_search_unsupported_provider.harn` pins `model: "gpt-4o"`**
  (phase 3 / harn#71) so it continues to error on `mode: "native"`
  after mock capability spoofing. The diagnostic still suggests
  `mode: "client"` as the escape hatch; the error text is unchanged.
- **Client-mode conformance tests now use `mode: "client"`
  explicitly** (phase 3 / harn#71). With mock spoofing a Claude 4.0+
  or GPT 5.4+ model, `mode: "auto"` would otherwise route through a
  native path. The tests name themselves `tool_search_client_*`;
  they now opt into the path they claim to cover.
- Non-Anthropic providers no longer error when the user opts into
  `tool_search`. The phase-1 "no silent degradation" diagnostic that
  previously pointed at harn#70 is replaced by the actual fallback
  behavior. The `mode: "native"` explicit-intent path still errors on
  providers without native support (its error message now suggests
  `mode: "client"` as the escape hatch).
- `tool_search_unsupported_provider.harn` conformance test adjusted
  to match the new behavior (only `mode: "native"` on mock still
  errors).

## v0.7.17

### Added

- **Skills are a first-class top-level form.** Adds `skill NAME { ... }`
  alongside `pipeline` / `fn` / `tool`. Each body entry is a
  `<field_name> <expression>` pair; lifecycle hooks
  (`on_activate fn() { ... }`, `on_deactivate`) are ordinary fn-literal
  expressions. The decl lowers to
  `skill_define(skill_registry(), NAME, { field: value, ... })` and
  binds the resulting registry dict to `NAME`. New stdlib module
  `crates/harn-vm/src/stdlib/skills.rs` exposes `skill_registry`,
  `skill_define`, `skill_list`, `skill_find`, `skill_select`,
  `skill_remove`, `skill_count`, `skill_describe`. `skill_define`
  validates known-key value shapes (`description`/`when_to_use`/
  `prompt`/`invocation`/`model`/`effort` as strings;
  `paths`/`allowed_tools`/`mcp` as lists) so typos raise at
  registration rather than at use. Attribute sugar `@acp_skill(name:
  "...", when_to_use: "...", invocation: "explicit", ...)` applied to
  a `fn` registers the fn as the skill's `on_activate` hook and lifts
  the remaining named args into the skill metadata. Covered by
  `conformance/tests/skill_decl.{harn,expected}`,
  `conformance/tests/attributes_acp_skill.{harn,expected}`, and
  `conformance/errors/skill_define_invalid.{harn,error}`. Coordinated
  updates to lexer (new `skill` keyword), parser (new `SkillDecl` AST
  with `fields: Vec<(String, SNode)>`), tree-sitter grammar + tests,
  VS Code syntax highlighter and snippets, spec, and quickref. Closes
  [#72](https://github.com/burin-labs/harn/issues/72).

- **Debugger M1–M4: DAP surface reaches protocol parity.** Adds the full
  Debug Adapter Protocol feature set needed for IDEs to drive Harn runs
  as first-class debug sessions. Capabilities advertised:
  `supportsLogPoints`, `supportsHitConditionalBreakpoints`,
  `supportsConditionalBreakpoints`, `supportsSetVariable`,
  `supportsSetExpression`, `supportsFunctionBreakpoints`,
  `supportsRestartFrame`, `supportsCompletionsRequest`,
  `supportsStepInTargetsRequest`, `supportsCancelRequest`,
  `supportsInvalidatedEvent`, plus Burin-namespaced
  `supportsBurinPromptProvenance`. `exceptionBreakpointFilters` expands
  to `{all, tool_error, llm_refusal, budget_exceeded, parse_failure}`
  with optional per-filter conditions (both legacy `filters` and
  DAP-1.58 `filterOptions` supported). Specific landings:
  - **#85 unified frame evaluator.** `Vm::evaluate_in_frame` /
    `set_variable_in_frame` / `restart_frame` with 10k-step budget,
    VM state snapshot/restore — powers hover, watches, conditional
    BPs, `setVariable` / `setExpression`, logpoint message rendering.
  - **#86 multi-thread readiness.** Per-`Debugger` thread registry
    seeded with `{1 → "main"}`; `threadStarted` / `threadExited`
    events; stepping / pause / exception events carry the live
    `threadId` instead of hardcoded 1.
  - **#87 logpoints.** `SourceBreakpoint.logMessage` renders via
    `{var}` interpolation without stopping.
  - **#88 hit-count breakpoints.** `hitCondition` parsed in
    `N / >=N / >N / %N` forms; counts reset on run enter and BP
    edits.
  - **#89 conditional breakpoints.** `SourceBreakpoint.condition`
    evaluated via the unified frame evaluator.
  - **#90 function breakpoints.** `setFunctionBreakpoints` stops on
    entry to any closure whose name matches; re-applied on launch so
    they survive relaunch.
  - **#91 `setVariable` / `setExpression`.** Mutate scope while
    paused, bypassing let-immutability via `VmEnv::assign_debug`.
  - **#92 `restartFrame`.** Rewinds `ip` and restores the
    `initial_env` snapshot captured at every call site.
  - **#93/#94 prompt provenance MVP.** `PromptSourceSpan` +
    thread-local `PROMPT_REGISTRY`; `render_with_provenance` builtin
    returns `{text, template_uri, prompt_id, spans}`; custom
    `burin/promptProvenance` and `burin/promptConsumers` DAP
    requests expose the registry over the wire.
  - **#102 triggered breakpoints.** `Breakpoint.triggeredBy: [id]`
    arms a BP only after a listed dependency BP has fired; armed
    state clears per run. Pattern: "break on the second turn's
    first `tool_error`".
  - **#108 `cancel` request.** Dispatches DAP cancel to both the
    `DapHostBridge`'s pending reverse-request waiter and a new
    `Vm::install_cancel_token` / `signal_cancel` cooperative token.
    The step loop polls on every instruction and unwinds with a
    `kind:cancelled:` `Thrown` that flows through the exception
    filter pipeline.
  - **#109 completions.** `Vm::identifiers_in_scope(frame_id)`
    unions frame locals with every registered builtin / async
    builtin; filtered by prefix, capped at 200.
  - **#110 `invalidated` events.** Helper builds the DAP
    `invalidated` event carrying areas + `current_thread_id`.
  - **#111 per-kind exception filters.** `extract_exception_kind`
    plus `exception_filter_matches` route `kind:<name>:` throws
    through the selected filter; stopped event carries
    `hitBreakpointIds: [kind]`.
  - **#112 `stepInTargets`.** Call-family ops on the current line
    enumerate per-target step-in IDs (`frame_id × 1e6 + index`).
- **Cross-template provenance chains (#96).** Every span emitted by
  `render_template_with_provenance` gets a `parent_span` +
  `template_uri`, so `include` traversal builds a walkable A→B→C
  chain. `burin/promptProvenance` surfaces the recursive chain plus
  `rootTemplateUri`, letting the IDE render cross-template
  breadcrumbs that click through to the inner source.
- **JSONL `AgentEvent` persistence (#103).** New `JsonlEventSink`
  writes an append-only `event_log-*.jsonl` stream with
  `{index, emitted_at_ms, frame_depth, event}` envelopes (flattened
  so `jq '.type'` still works). 100 MB rotation, `Drop`-flush,
  errors swallowed so a broken sink never kills a session.
  `agent_sessions::open_or_create` auto-registers the sink when
  `HARN_EVENT_LOG_DIR` is set. Foundation for the scrubber,
  branch-replay, and jump-to-render IDE actions.
- **Branch-replay via `fork_at` (#105).** `agent_sessions::fork_at`
  forks a source session and truncates the new session's transcript
  to the first N messages, so the scrubber can rewind to a past
  event index and spawn a live sibling whose next decision diverges
  cleanly. Subscribers are not carried over — parent fanout
  consumers don't double-receive.
- **Prompt render-index registry (#106).** Thread-local
  `PROMPT_RENDER_INDICES` map from `prompt_id` → `[ordinal…]`, plus
  new `prompt_mark_rendered(prompt_id) → int` host builtin that
  pipelines call right before handing a rendered prompt to
  `llm_call`. `burin/promptConsumers` now surfaces the ordinal list
  so the IDE template gutter can jump to the next matching render
  event.
- **Tool Vault foundation: native progressive tool disclosure on Anthropic.**
  Mark individual tools with `defer_loading: true` via `tool_define`
  (or the dict form) and opt a call into progressive disclosure with a
  new `tool_search: "bm25" | "regex" | {variant, mode, always_loaded}`
  option on `llm_call` / `agent_loop`. On Claude Opus/Sonnet 4.0+ and
  Haiku 4.5+, Harn emits native `defer_loading: true` in the tool JSON
  and prepends the appropriate `tool_search_tool_{bm25,regex}_20251119`
  server tool. Schemas stay in the API prefix (so prompt caching
  remains warm) but out of the model's context until the model
  discovers them. Typical token reductions of ~85% for large tool
  catalogues. Phase 1 of the Harn Skills & Tool Vault series; see
  harn#69 for the full plan and follow-up issues.
- **Provider capability surface.** The `LlmProvider` trait gains
  `supports_defer_loading(&str) -> bool` and
  `native_tool_search_variants(&str) -> &[&str]`, letting Harn decide
  per-provider per-model whether native progressive disclosure is
  available. Anthropic implements both; OpenAI lands in harn#71.
- **Transcript events for tool search.** Anthropic `server_tool_use`
  and `tool_search_tool_result` response blocks are now parsed into
  structured `tool_search_query` and `tool_search_result` events in
  the run record — replay / eval can reconstruct which tools got
  promoted when without re-running the call.
- **Pre-flight validation.** Passing `tool_search` with every tool
  set to `defer_loading: true` errors before the API call, matching
  Anthropic's documented 400. `defer_loading` itself is type-checked
  at `tool_define` so typos fail fast.

### Breaking

- Non-Anthropic providers (or Anthropic models older than 4.0 Opus/
  Sonnet / 4.5 Haiku) error with a precise diagnostic when
  `tool_search` is requested, pointing at harn#70 for the upcoming
  client-executed fallback. This is intentional (no silent
  degradation); client fallback makes the feature provider-agnostic
  in the next phase.
- **Distributive instantiation of generic type aliases.** Applying a
  generic `type F<T> = ...` alias to a closed union now expands into a
  union of per-variant instantiations rather than leaving the union in
  a single `T` slot. Concretely, for

  ```harn
  type Action = "create" | "edit"
  type ActionContainer<T> = { action: T, process_action: fn(T) -> nil }
  ```

  the type `ActionContainer<Action>` now resolves as
  `ActionContainer<"create"> | ActionContainer<"edit">`, which lets a
  `fn("create") -> nil` handler flow into the `"create"` branch without
  running aground on the contravariance of the function parameter.
  This is the pattern TypeScript rejects in the classic
  `Array<ActionContainer<Action>>` playground example; Harn now handles
  it soundly via distribution at alias-application time in
  `crates/harn-parser/src/typechecker/inference/subtyping.rs`. No new
  syntax or keyword required — distribution is an implementation of
  existing alias-application semantics.
- **Discriminator narrowing on tagged shape unions.** A union of two or
  more dict shapes that share a literal-typed, distinct-per-variant
  field is now a *tagged shape union*. Matching on that field
  (`match obj.<tag>`) or testing it (`if obj.<tag> == "value"` /
  `else`) narrows `obj` to the matching variant inside each arm or
  branch. The discriminant is auto-detected — there is no privileged
  field name, `kind` and `type` and `op` and any other shared
  literal-typed field all work identically. Plain literal unions
  (`"pass" | "fail" | "unclear"`) gain the same exhaustive `match`
  treatment as enums.
- **Reserved keywords are now legal shape-type field names.**
  `{type: "click", x: int, y: int}` parses in type position as well
  as in dict-literal and property-access position. Closes a small
  asymmetry that previously forced workarounds for `type`-tagged
  shape unions.
- **Conformance and quickref pin the surface contract.** New
  conformance tests `shape_union_discriminator_forms` (parse +
  format invariants across `kind`, `type`, and `op` discriminants
  plus pure literal unions) and `shape_union_discriminator_narrow`
  (end-to-end narrowing in match arms and `if` branches). The
  `harn-scripting` skill autoloads `docs/llm/harn-quickref.md`,
  which now ships a "Discriminated unions & distribution" block
  with copy-paste-ready examples for all three forms.
- **Residual + post-distribution narrowing conformance.** Two new
  fixtures pin behaviour that was previously only covered by
  typechecker unit tests:
  `shape_union_not_equal_narrowing.{harn,expected}` exercises the
  residual narrow on `if obj.<tag> != "value"` (truthy branch
  narrows to the union of the other variants; else branch narrows
  to the single matched variant). `shape_union_post_distribution.
  {harn,expected}` exercises `Container<A | B>` distributing to
  `Container<A> | Container<B>` and then going through the tagged-
  shape-union discriminator-narrowing path end-to-end.
- **LSP: tagged shape union hover expands each variant.** Hovering
  on a variable typed as a tagged shape union (two-plus dict
  shapes) previously collapsed onto a single wide line. The hover
  handler in `crates/harn-lsp/src/handlers.rs` now invokes
  `format_union_shapes_expanded` (new in `symbols.rs`) to render
  each variant on its own block with field-per-line formatting,
  separated by `|` — matching the existing `format_shape_expanded`
  style used for single shapes.
- **LSP: completion of discriminator literal values inside `match`.**
  When the cursor sits in arm-pattern position of a `match obj.<tag>
  { … }` block and `obj` resolves to a tagged shape union, the
  completion list now surfaces each distinct discriminator literal
  as an `ENUM_MEMBER` item (with the matched variants for arms
  already present filtered out). Implemented via an AST walk in
  `discriminator_value_completions`; the type-alias chain is
  resolved through `resolve_type_alias_from_ast` so `m: Msg` with
  `type Msg = Ping | Pong` is treated identically to an inline
  union.
- **LSP: quick-fix to add missing `match` arms.** The typechecker
  now attaches a structured `DiagnosticDetails::NonExhaustiveMatch
  { missing: Vec<String> }` payload to non-exhaustive-match errors
  on enums, tagged shape unions, and literal unions. The LSP code-
  action provider reads it and synthesises a `WorkspaceEdit` that
  inserts one stub arm per missing variant
  (`<literal> -> { unreachable("TODO: handle <literal>") }`),
  indented to match the match body's closing brace. Marked
  `isPreferred: true` so the client surfaces it first.
- **Or-patterns in `match` arms (`pat1 | pat2 -> body`).** A single
  arm may list two or more literal alternatives separated by `|`;
  the shared body runs when any alternative matches, and each
  alternative contributes to exhaustiveness coverage independently.
  Inside the arm, the matched variable is narrowed to the *union*
  of the alternatives' single-literal narrowings — on a literal
  union this is a sub-union, on a tagged shape union it is a union
  of the matching shape variants. Guards compose naturally:
  `1 | 2 | 3 if n > 2 -> …` runs the body only when some
  alternative matched *and* the guard held. Alternatives are
  restricted to literal patterns (string, int, float, bool, nil)
  and the wildcard `_`; identifier-binding and destructuring
  alternatives are rejected with a specific diagnostic. Lowering
  mirrors the existing literal-arm shape in `crates/harn-vm/src/
  compiler/patterns.rs`, so no new opcodes were needed. Pinned by
  conformance tests `match_or_pattern` (literal-union + guard
  combinations) and `shape_union_or_pattern` (narrowing into a
  two-variant union on a tagged shape union), plus typechecker
  tests in `exhaustiveness.rs` and `narrowing.rs`. Tree-sitter
  grammar adds an `or_pattern` rule, pinned by the new
  `match_arms` corpus.

### Breaking — typechecker

- **Non-exhaustive `match` is a hard error.** A `match` that omits
  enum variants, tagged-shape-union variants, named-type union
  members, or literal-union members must add the missing arm or
  end with a wildcard `_ -> { … }` arm. `if/elif/else` chains stay
  intentionally partial; opt into exhaustiveness by ending the
  chain with `unreachable("…")`, which still flows through the
  warning-level `check_unknown_exhaustiveness` path.

### Removed

- **`auto.harn` `< 40-char` safety net (#107 follow-up).** The fallback
  that routed short inputs through `chat_reply` is gone; explanation
  intents classify as `qa` upstream and take the dedicated
  `qa_reply` path. An empty result now surfaces the real pipeline
  state honestly instead of masking bugs.

### Deferred (separate follow-up)

- **Canonical ADT surface syntax** — the planned
  `type Action = Create { x: int } | Edit { y: int }` form, with a
  unified internal `TypeExpr::Adt` representation behind it, is
  intentionally *not* in this changeset. The user-visible
  capabilities the canonical syntax was meant to deliver
  (discriminator narrowing, exhaustiveness, distributive generic
  instantiation, schema oneOf via the existing enum path) are all
  in place via tagged shape unions, legacy enums, and alias
  distribution; the surface change is additive sugar that requires
  coordinated parser/VM/fmt/LSP/tree-sitter/VS Code work and
  warrants its own PR.

### Fixed

- **Tagged shape unions with `Named`-alias members now narrow.**
  `type Ping = {kind:"ping",…}; type Msg = Ping | {kind:"pong",…}`
  previously lost discriminator narrowing: the bare-`Shape` check in
  `discriminant_field` rejected the `Named("Ping")` member on sight,
  so `match m.kind` and `if m.kind == "ping"` both degraded to the
  raw `Msg` type inside the branch. `resolve_union_shape_members`
  (new helper in
  `crates/harn-parser/src/typechecker/inference/flow.rs`) resolves
  the `Named`-alias chain in each union member before
  `discriminant_field` / `narrow_shape_union_by_tag` inspect the
  shapes. Pinned by conformance
  `shape_union_named_alias_member.{harn,expected}` and typechecker
  tests `test_match_narrows_through_named_alias_member` /
  `test_if_narrows_through_named_alias_member` in `narrowing.rs`.
- **Match-arm guard no longer consumes the match value on fail.**
  When a literal-pattern match arm's guard evaluated to false, the
  emitted bytecode over-popped and consumed the match value before
  the next arm's `Dup`, surfacing as a runtime
  "Stack underflow" once a subsequent arm ran. The guard-fail path
  now falls through to the shared trailing `Pop` (same as the
  match-fail path), matching the discipline used by dict/list
  destructuring arms. The new or-pattern lowering follows the same
  corrected shape.
- **Bare function references now carry their full `fn(...)` type.**
  Previously, a top-level (or nested) function used as a plain value
  (e.g. inside a dict literal) inferred as `None`, which collapsed to
  `nil` at the surrounding inference site. A subsequent assignment into
  a typed slot then failed with a misleading "got nil" diagnostic. The
  typechecker now falls back from `scope.get_var` to `scope.get_fn`
  when resolving bare identifiers, projecting the function signature
  into a proper `FnType { params, return_type }`.

## v0.7.16

### Fixed

- **Debugger: breakpoints on the entry script now actually stop execution.**
  `harn-dap`'s `compile_program` was calling `harn_vm::compile_source`,
  which produces a `Chunk` without a `source_file` set. Because
  `Vm::breakpoint_matches` keys its lookup on the current frame's
  `chunk.source_file`, path-keyed breakpoints from a DAP client (VS Code,
  Burin, …) could never match — only the wildcard (empty-string) set
  fired, which clients don't emit in practice. Imported modules already
  got the right tag via `compile_fn_body`; the entry chunk now gets it
  too. `test_breakpoint_stop` is tightened to demand `reason="breakpoint"`
  so the regression can't recur silently.

## v0.7.15

### Changed

- **Internal: finished splitting the remaining oversized source files
  into focused modules.** v0.7.13's `typechecker/` split continues with
  six more multi-thousand-line files, each now a directory of focused
  submodules. Public API surface is preserved through `pub(crate) use`
  re-exports in each `mod.rs`, so downstream crates and call sites are
  unchanged. Bytecode output and all conformance/unit/portal/
  tree-sitter tests are byte-for-byte identical (472/472 conformance,
  164/164 parser, 130/130 harn-cli, 16/16 tree-sitter). Every resulting
  file is under ~1,200 lines.
  - `crates/harn-parser/src/parser.rs` (3,038 lines) → `parser/`
    module split into `decls`, `error`, `expressions`, `patterns`,
    `state`, `statements`, and `types` (closes #41).
  - `crates/harn-vm/src/compiler.rs` (3,631 lines) → `compiler/`
    module split into `closures`, `concurrency`, `decls`, `error`,
    `error_handling`, `expressions`, `patterns`, `pipe`, `state`,
    `statements`, `tests`, and `yield_scan` (closes #38).
  - `crates/harn-vm/src/stdlib/workflow.rs` (2,240 lines) →
    `workflow/` module split into `artifact`, `convert`, `guards`,
    `map`, `policy`, `register`, `stage`, `tests`, and `usage`
    (closes #45).
  - `crates/harn-cli/src/commands/portal.rs` (3,070 lines) → `portal/`
    module split into `assets`, `dto`, `errors`, `handlers/`,
    `highlight`, `launch`, `llm`, `query`, `router`, `run_analysis`,
    `state`, `transcript`, and `util` (closes #40).
  - `crates/harn-cli/src/commands/check.rs` (3,505 lines) → `check/`
    module split into `bundle`, `check_cmd`, `config`, `fmt`,
    `host_capabilities`, `imports`, `lint`, `mock_host`, `outcome`,
    `preflight`, and `tests` (closes #39).
  - `crates/harn-lint/src/lib.rs` (2,652 lines) → focused modules:
    `diagnostic`, `decls`, `naming`, `harndoc`, `linter` (+
    `linter/walk`), and one file per source-aware rule under `rules/`
    (`blank_lines`, `file_header`, `import_order`, `trailing_comma`)
    (closes #43).

## v0.7.14

### Fixed

- **Lexer: multi-line `${…}` interpolation now tracks line numbers.**
  Inside a single-line string, the `${…}` expression can itself span
  multiple physical lines (e.g. `${render(\n  "a",\n  b,\n)}`). The lexer
  consumed those inner newlines without advancing `self.line`, so every
  token after such a string reported a line number that was too low —
  by the number of newlines consumed inside the interpolation. Downstream
  `missing-harndoc` lint spans pointed at the wrong declarations. Matches
  the long-standing behavior of the multi-line (`"""…"""`) string lexer,
  which already handled this correctly.
- **Formatter: doc comment between `@attr` and `pub fn` is preserved.**
  Placing `/** … */` between an attribute and its declaration (the order
  the `missing-harndoc` rule requires when both are present) used to
  drop the doc and re-emit it above the *next* top-level item. The
  formatter now emits comments in the `last_attr.span.line + 1 ..
  inner.span.line` range before recursing into the inner declaration.

## v0.7.13

### Changed

- **Anthropic provider: Claude Opus 4.7 compatibility.** The Anthropic
  request builder now recognizes Claude model generations and applies
  Opus 4.7's breaking API changes automatically:
  - Sampling parameters (`temperature`, `top_p`, `top_k`) are stripped
    from request bodies for Opus 4.7+ models (Anthropic returns HTTP 400
    on non-default values). A one-time `llm.sampling` warning surfaces
    when we drop them.
  - `thinking: {type: "enabled", budget_tokens: N}` payloads are
    transparently rewritten to `thinking: {type: "adaptive"}` for Opus
    4.7+ models (extended thinking was removed from that generation).
    Pipeline authors don't need to special-case the API change; the
    provider layer handles it and logs once per model.
  - The pre-existing prefill gate (deprecated in Claude 4.6) is now
    generation-aware: it fires for every `claude-*-4.6+` model in either
    dash (`claude-opus-4-7`) or dotted (`anthropic/claude-opus-4.7`)
    form, replacing the previous hardcoded family-name list.
- **Internal: `harn-parser` typechecker split into a `typechecker/`
  module.** The 7,782-line `typechecker.rs` is now a directory of
  focused files (`scope`, `format`, `union`, `exits`,
  `schema_inference`, `binary_ops`, and an `inference/` sub-module split
  by node-kind family). The public API is re-exported from
  `typechecker/mod.rs`, so no downstream crate needed edits. Docs-snippet
  coverage was also extended: 9 `harn` fences across `concurrency`,
  `error-handling`, `language-basics`, `language-spec`, and
  `scripting-cheatsheet` now include the helper stubs they reference so
  `harn check` passes under the stricter cross-module undefined-call
  gate added in v0.7.12.

## v0.7.12

### Added

- **Static cross-module undefined-call errors.** `harn check`,
  `harn run`, `harn bench`, and the LSP now share one recursive module
  graph built by `harn-modules`. When every import in a file resolves,
  the typechecker treats any call target that is not a builtin, local
  declaration, struct constructor, callable variable, or imported
  symbol as an error (`call target ... is not defined or imported`)
  instead of letting the VM discover it at runtime. If any import is
  unresolved, the stricter check is skipped for that file so one broken
  import does not cascade into a flood of false positives.

### Changed

- **DRY cross-module primitives.** LSP go-to-definition now walks the
  same `harn_modules::ModuleGraph` used by check/lint, instead of its
  own duplicated import-walking logic. `harn-lsp`, `harn-lint`, and the
  CLI all consume a single `harn_modules::build(...)` call per entry
  file, which transitively loads every reachable `.harn` module once.

## v0.7.11

### Added

- **DAP pause-during-run.** `pause` now interrupts a program that is
  actively executing instead of being a no-op during runs. The adapter's
  main loop interleaves VM steps with non-blocking drains of the DAP
  request channel, so `pause`, `setBreakpoints`, and `disconnect`
  arriving mid-run are serviced between steps. On `pause`, the next step
  tick stops with `reason: "pause"` without advancing the VM.
- **DAP progress events during runs.** `configurationDone` now emits a
  `progressStart` so the IDE shows a "Running…" indicator, with
  throttled `progressUpdate` ticks (roughly every 256 VM steps) carrying
  the current line. Progress is ended on every stop path (breakpoint,
  pause, exception, terminate, disconnect) so the IDE's liveness
  indicator clears cleanly.
- **`harnPing` DAP request.** Lightweight liveness check the IDE can
  send to distinguish "wedged" from "actively stepping". Responds with
  `{state, running, stopped}` derived from the current debugger state.

### Fixed

- **DAP `disconnect` no longer waits on in-flight host calls.**
  `disconnect` now cancels any pending `DapHostBridge` reverse-request
  waiters with a synthetic failure carrying `reason: "cancelled:
  disconnect"`, tears down the VM, and ends any active progress event.
  Previously, a host call in flight at disconnect time kept the script
  blocked until the 60s reverse-request timeout. Scripts now unwind
  promptly when the IDE walks away.

## v0.7.10

### Fixed

- **DAP breakpoints scoped to the requesting source file.** Previously,
  `setBreakpoints` cleared *all* breakpoints across every file before
  re-installing the new set, and the VM matched on raw line numbers
  with no regard for which source file was executing — so a breakpoint
  at line 10 of `auto.harn` would also fire when an imported library
  hit its own line 10. The DAP adapter now retains breakpoints from
  files other than the one named in the request (per spec), and the
  VM stores breakpoints in a per-file map (`set_breakpoints_for_file`)
  with a backwards-compatible wildcard form (`set_breakpoints`, empty
  key). A path-suffix fallback handles relative-vs-absolute path drift
  between IDE and runtime. Multi-file pipelines now break exactly where
  the user asked.

### Public API

- `harn_vm::Vm::set_breakpoints_for_file(file, lines)` — replace the
  breakpoint set for one source file. Existing
  `set_breakpoints(lines)` is preserved as a wildcard shorthand.

## v0.7.9

### Added

- **DAP host-call bridge (`harnHostCall` reverse request).** The
  debugger now round-trips unhandled `host_call(capability, operation,
  ...)` ops to the DAP client as reverse requests, mirroring the DAP
  `runInTerminal` pattern. On `success: true`, the adapter returns the
  body's `value` (or the whole body when `value` is absent); on
  `success: false`, it raises `VmError::Thrown(message)` so scripts can
  `try`/`catch` it. The stdin reader runs on a dedicated thread so the
  bridge can block on reply channels without starving the main message
  loop, and adapter-initiated seqs (forward responses + reverse
  requests) share one counter so every frame stays unique. Capabilities
  advertise the new `supportsHarnHostCall: true` field — clients that
  do not set the matching capability still work and simply see the
  standalone fallbacks.
- **`HostCallBridge` trait in `harn-vm`.** New public surface
  (`harn_vm::HostCallBridge`, `set_host_call_bridge`,
  `clear_host_call_bridge`) lets embedders (debug adapters, IDE hosts,
  CLI wrappers) satisfy capability/operation pairs that harn-vm itself
  does not know how to handle. `Ok(None)` falls through to the built-in
  fallbacks; `Ok(Some(_))` is the result; `Err(VmError::Thrown(_))`
  surfaces as a Harn exception. The bridge is consulted after mock
  matching and before built-in match arms, so embedders can override
  anything and equally punt on anything.
- **Standalone `workspace.project_root` / `workspace.cwd` fallbacks.**
  Pipelines call `host_call("workspace", "project_root", {})` very
  early, so the VM now answers these ops even when no embedder bridge
  is installed. `project_root` prefers `HARN_PROJECT_ROOT` and falls
  back to `std::env::current_dir()`; `cwd` always returns the current
  working directory. Keeps debug-launched scripts unblocked when the
  IDE has not wired `harnHostCall` through yet.
- **LLM-call telemetry as DAP `output` events.** The debugger enables
  harn-vm's agent trace, drains `AgentTraceEvent::LlmCall` entries
  between VM steps, and forwards them as DAP `output` events with
  `category: "telemetry"` and a JSON body (`call_id`, `model`,
  `prompt_tokens`, `completion_tokens`, `cache_tokens`, `total_ms`,
  `iteration`). Other trace kinds are skipped for now — the IDE
  consumes only LLM telemetry.
- **Cross-file go-to-definition in the LSP.** `textDocument/definition`
  now walks the document's `import` declarations, resolves each path
  with the same relative + `.harn/packages/` lookup order harn-vm
  uses, parses the imported file, builds its symbol table, and
  returns the first matching pipeline / function / variable / struct /
  enum / interface. Selective imports that name the target symbol are
  searched first so the highest-confidence hit wins.

## v0.7.8

### Added

- **Typed pipeline returns (`pipeline name() -> TypeExpr { ... }`).**
  Pipelines can now declare a return type with the same `-> TypeExpr`
  syntax as functions. The type checker validates every `return <expr>`
  against the declared type, turning the Harn→ACP/A2A boundary into a
  type-checked contract instead of relying on the host bridge as the
  only enforcement point. A new `std/acp` stdlib module ships
  canonical ACP envelope type aliases (`SessionUpdate`,
  `AgentMessageChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`,
  `PipelineResult`) plus constructor helpers
  (`agent_message_chunk`/`tool_call`/`tool_call_update`/`plan`).
  Public pipelines without an explicit return type emit the
  `pipeline-return-type` lint warning as a one-release deprecation
  window; well-known entry names (`default`, `main`, `auto`, `test`)
  are exempt. Resolves
  [#31](https://github.com/burin-labs/harn/issues/31).
- **DAP `pause` request and `supports_terminate_request` capability.**
  The debugger now handles the DAP `pause` request by flipping the VM
  into step-in mode and emitting a `stopped` event when execution is
  already halted, giving IDEs a meaningful pause affordance.
  Capabilities now advertise `supports_terminate_request: true`.

### Changed

- **`cyclomatic-complexity` default bumped from 10 → 25** and made
  configurable via `[lint].complexity_threshold` in `harn.toml`. The
  old default treated any function with more than ten decision points
  as suspect, which turned the rule into the dominant lint signal in
  real Harn projects (137 of 210 warnings in `burin-code`, 65%). 25
  matches Clippy's `cognitive_complexity` default and splits the
  difference between ESLint (20) and gocyclo (30); Harn counts
  `&&`/`||` per operator, so real-world Harn functions score a notch
  higher than in tools that only count control-flow nodes. The
  diagnostic now names the `@complexity(allow)` escape hatch and the
  `harn.toml` knob. Note: the originally-proposed `harn lint --fix`
  for cyclomatic complexity was dropped after inspection — none of
  the mechanical transforms (early-return flattening, De Morgan on
  nested `if`-returns, redundant-`else` elimination) actually reduce
  the cyclomatic score, since guards and `&&`/`||` each cost 1. Those
  transforms improve cognitive complexity / nesting depth and may
  ship under a separate future lint. Resolves
  [#29](https://github.com/burin-labs/harn/issues/29).

## v0.7.7

### Added

- **Attribute / decorator surface (`@name(...)`).** Top-level
  declarations (`pipeline`, `fn`, `tool`, `struct`, `enum`, `type`,
  `interface`, `impl`) can now carry one or more attributes. The
  initial set is:
  - `@deprecated(since: "X", use: "Y")` — type-checker warning at
    every call site, with both args optional.
  - `@test` — marks a `pipeline` as a test entry point, recognized
    by `harn test conformance` alongside the legacy `test_*` naming
    convention.
  - `@complexity(allow)` — suppresses the `cyclomatic-complexity` lint
    on the attached function.
  - `@acp_tool(name: ..., kind: ..., side_effect_level: ..., ...)` —
    desugars to a runtime `tool_define(...)` call with the attached
    function bound as the handler and named args (other than `name`)
    lifted into the `annotations` dict so `ToolAnnotations` flows
    through ACP/A2A unchanged.

  Attribute arguments are restricted to literal values (strings,
  numbers, `true`/`false`/`nil`, bare identifiers) — there is no
  runtime evaluation. Unknown attribute names produce a type-checker
  warning so misspellings surface at check time. Documented in
  `spec/HARN_SPEC.md` ("Attributes" section) and the quickref.
  Resolves [#30](https://github.com/burin-labs/harn/issues/30).

## v0.7.6

### Added

- **Stdlib polish: `llm_call_safe`, `read_file_result`, `env_or`,
  `with_rate_limit`.** Four small builtins that eliminate repetitive
  ceremony in grading/bench/eval scripts. `llm_call_safe(prompt,
  system?, opts?)` is a non-throwing envelope around `llm_call`
  returning `{ok, response, error: {category, message} | nil}`, with
  `error.category` drawn from the canonical `ErrorCategory` string
  set (`"rate_limit"`, `"timeout"`, `"overloaded"`,
  `"transient_network"`, `"schema_validation"`, etc).
  `read_file_result(path)` is a non-throwing sibling of `read_file`
  returning `Result.Ok(content)` / `Result.Err(message)` and sharing
  the same content cache. `env_or(key, default)` collapses the
  `let v = env(K); if v { v } else { default }` pattern. `with_rate_limit(provider, fn, opts?)`
  acquires a sliding-window permit and retries the closure with
  exponential backoff on retryable categories (`rate_limit`,
  `overloaded`, `transient_network`, `timeout`) — composes with
  `HARN_RATE_LIMIT_<PROVIDER>` env vars and `llm_rate_limit(...)`.
  Resolves [#28](https://github.com/burin-labs/harn/issues/28).
- **`llm_mock` error injection.** `llm_mock({error: {category, message}})`
  now synthesizes a `VmError::CategorizedError` on match instead of an
  `LlmResult`, so `try { llm_call(...) }`, `error_category`,
  `llm_call_safe`'s error envelope, and `with_rate_limit`'s retry loop
  all have deterministic test coverage. Unknown category strings are
  rejected at registration time.

## v0.7.5

### Added

- **Generic inference across schema-driven builtins.** `llm_call`,
  `llm_completion`, `schema_parse`, `schema_check`, and `schema_expect`
  now carry real generic signatures keyed on a new `Schema<T>` type
  constructor. User-defined wrappers inherit the same narrowing
  without any typechecker special case:

  ```harn
  fn grade<T>(prompt: string, schema: Schema<T>) -> T {
    let r = llm_call(prompt, nil,
      {output_schema: schema, output_validation: "error",
       response_format: "json"})
    return r.data
  }

  let out: GraderOut = grade("Grade this", schema_of(GraderOut))
  // out.verdict / out.summary narrow without schema_is guards.
  ```

  The `Schema<T>` type constructor denotes a runtime schema value
  whose static shape is `T`. When a parameter is typed `Schema<T>`,
  the argument's value node (a type-alias identifier, `schema_of(T)`,
  or an inline JSON-Schema dict) binds the generic parameter,
  threading the narrowed type through the call's return type. The
  hand-rolled `extract_llm_schema_from_options` narrowing is
  removed in favor of this generic dispatch, and user generic
  functions use the same node-walking inference. Runtime
  `schema_of(T)` is unchanged. Resolves
  [#33](https://github.com/burin-labs/harn/issues/33).

## v0.7.4

### Added

- **Comprehensive variance (`in T` / `out T`).** Type parameters on
  user-defined generics may now be marked with `in` (contravariant)
  or `out` (covariant). Unannotated parameters default to
  **invariant** — strictly safer than the previous implicit
  covariance. The subtype relation is now polarity-aware: built-in
  `iter<T>` is covariant, `list<T>` and `dict<K, V>` are invariant
  (mutable), and function types are contravariant in their parameters
  and covariant in their return type. Declaration sites are checked
  too: `type Box<out T> = fn(T) -> int` is rejected because `T`
  appears in a contravariant position. Generic type aliases
  (`type Foo<T> = ...`) are now supported in the parser. See the
  spec's "Subtyping and variance" section. Resolves
  [#34](https://github.com/burin-labs/harn/issues/34).
- **`fn`-type parameter contravariance fix.** Function-type
  parameter compatibility was previously checked covariantly, which
  let `fn(int) -> R` stand in for an expected `fn(float) -> R` —
  unsound, since the caller may hand the closure a float it cannot
  receive. Parameters are now checked contravariantly per the
  variance rewrite above; `fn(float)` correctly substitutes for
  `fn(int)` but not the reverse.
- **Exhaustive narrowing on `unknown`.** The type checker now tracks
  which concrete `type_of` variants have been ruled out on each
  flow path for every `unknown`-typed variable. When control flow
  reaches a never-returning site — `unreachable()`, a `throw`, or a
  call to a user-defined function with return type `never` — the
  checker warns if the coverage set is non-empty but incomplete,
  naming the uncovered variants. Plain `return` fallthroughs are
  not exhaustiveness claims and stay silent, and a bare `throw`
  with no prior `type_of` narrowing also stays silent. Resolves
  [#27](https://github.com/burin-labs/harn/issues/27).
- **`try*` finally-pop fix.** Compiler now unconditionally pops the
  one-value-per-block leftover after a finally body, so a `finally`
  ending in a non-value statement (e.g. `x = x + 1`) no longer leaks
  a stray `nil` onto the stack of the surrounding expression. This
  was latent in `try { ... } finally { x = x + 1 }` used in
  expression position; surfaced while wiring the new `try*` operator.
- **`try* EXPR` — rethrow-into-catch operator.** Replaces the
  `try { foo() } / guard is_ok else / unwrap` boilerplate with a
  one-token prefix form. `try* EXPR` evaluates `EXPR` and, on a thrown
  error, runs every `finally` block between the rethrow site and the
  innermost catch handler exactly once before rethrowing the original
  value into that handler. On success it evaluates to `EXPR`'s value
  with no `Result` wrapping. `try*` requires an enclosing function
  (`fn`, `tool`, or `pipeline`) — using it at module top level is a
  compile error. Distinct from postfix `?` (which early-returns
  `Result.Err(...)` from a Result-returning function); use `try*` when
  you want a thrown error to land in an enclosing `try { ... } catch`
  rather than be returned as a Result. Resolves
  [#26](https://github.com/burin-labs/harn/issues/26).
- **Schema-as-type: unified `type` aliases with `output_schema` /
  `schema_*` builtins.** A `type` alias can now drive both static
  type-checking and runtime schema validation from a single source of
  truth. `schema_of(T)` lowers a type alias to a JSON-Schema dict at
  compile time, and the same alias identifier is accepted as the schema
  argument of `schema_is` / `schema_expect` / `schema_parse` /
  `schema_check` / `is_type` / `json_validate`, and as the value of
  `output_schema:` in an `llm_call` options dict. Narrowing on
  `schema_is(x, T)` refines `x` to `T` in the truthy branch. The type
  grammar now admits string- and int-literal types in unions
  (`"pass" | "fail" | "unclear"`, `0 | 1 | 2`), emitted as canonical
  `{type, enum}` JSON Schema so schemas are compatible with both
  structured-output validators and ACP `ToolAnnotations.args`. Resolves
  [#25](https://github.com/burin-labs/harn/issues/25). See
  `docs/src/migrations/schema-as-type.md` for the migration guide.
- **`harn check` workspace manifest (`[workspace].pipelines`).** The CLI now
  walks upward from a target file (stopping at the first `.git` boundary) to
  find the nearest `harn.toml` and honors a new `[workspace]` section. Run
  `harn check --workspace` to type-check every `.harn` file under the listed
  pipeline roots in a single invocation without threading per-file
  `--host-capabilities` flags. See `spec/HARN_SPEC.md` → "Workspace manifest
  (`harn.toml`)" and `docs/src/cli-reference.md` for the schema.
- **Preflight diagnostics separated from type errors.** Preflight diagnostics
  from `harn check` are now reported under a distinct `preflight` category so
  IDE filters and CI log scrapers can route them separately from type-checker
  output. Two new knobs control them: `[check].preflight_severity = "error"
  | "warning" | "off"` (overridable with the new `--preflight <severity>`
  flag), and `[check].preflight_allow = ["capability.operation", ...]` which
  accepts exact matches, `capability.*` wildcards, bare capability names, or
  a blanket `*`. The existing `--host-capabilities` flag continues to work as
  a per-invocation override of `[check].host_capabilities_path`. Resolves
  [#24](https://github.com/burin-labs/harn/issues/24).

### Breaking

- **Unannotated user generics are now invariant by default**
  instead of implicitly covariant. Code that relied on
  `MyBox<int>` flowing into `MyBox<float>` must add an explicit
  `out T` annotation to the declaration (and ensure `T` only
  appears in covariant positions). See
  [#34](https://github.com/burin-labs/harn/issues/34).
- **`list<T>` and `dict<K, V>` are now invariant.** `list<int>` no
  longer flows into `list<float>`, and `dict<string, int>` no
  longer flows into `dict<string, float>`. Mutable containers
  cannot be soundly covariant on writes; use `iter<T>` for
  read-only sequences that still need element-type widening.
- **`fn`-type parameters are now contravariant.** `fn(int)` no
  longer satisfies an expected `fn(float)`. The reverse direction
  (`fn(float)` standing in for `fn(int)`) is the new accepted form.

## v0.7.3

### Added

- **`any` is now a true top type; `unknown` added as the safe top.**
  Previously `any` behaved like a plain named type that only matched
  itself — assigning `nil` to an `any`-typed slot raised
  `'x' declared as any, but assigned nil`. With this release, `any`
  accepts every concrete type and flows back out to every concrete
  type with no narrowing required (the explicit escape hatch). A new
  `unknown` type fills the TypeScript-style "safe top" role: every
  value is assignable to `unknown`, but `unknown` is not assignable
  back to any concrete type without narrowing via
  `type_of(x) == "..."` or `schema_is(x, Shape)`. `unknown` is the
  preferred annotation for values arriving from untrusted boundaries
  (parsed JSON, LLM responses, dynamic dicts). `unknown` interoperates
  with `any` in both directions. See the new `### The any type` and
  `### The unknown type` sections in `spec/HARN_SPEC.md`, and the
  `Typing: any vs unknown vs no annotation` block in
  `docs/llm/harn-quickref.md`. Conformance coverage lives in
  `conformance/tests/any_top_type.*`, `unknown_safe_top.*`,
  `unknown_requires_narrowing.*`, and `unknown_narrowing.*`.

## v0.7.2

### Added

- **`try/catch/finally` as an expression.** `let v = try { work() } catch (e)
  { fallback }` now binds directly — the form evaluates to the try body's
  tail value on success or the catch handler's tail value on a caught throw,
  without routing through `Result` helpers. A trailing `finally { ... }`
  runs once for side-effect only and does not contribute a value. Typed
  catches (`catch (e: AppError) { ... }`) still rethrow past the expression
  when the thrown error's type does not match the filter, so the `let`
  binding is never established. The bare `try { body }` form continues to
  wrap in `Result<T, E>` — only adding `catch` or `finally` switches to the
  handled-expression shape. See `docs/src/error-handling.md` and
  `spec/HARN_SPEC.md`.
- **Tree-sitter grammar: `try` is now a unified expression rule.** The
  grammar previously exposed `try_catch_statement` and `try_expression` as
  separate rules; both forms — statement-position `try/catch/finally` and
  expression-position `try`, `try/catch`, `try/finally`, and
  `try/catch/finally` — are now modeled as one `try_expression` rule with
  optional `catch` and `finally` clauses. This removes a parse-time
  split that no longer matched runtime semantics and keeps the grammar
  aligned with the parser.

### Fixed

- **`finally` runs exactly once per control-flow path.** A longstanding
  compiler bug pre-ran pending `finally` bodies when lowering `throw`,
  and then ran them *again* after a local `catch` finished — so on the
  caught-throw path every `finally` fired twice, and when a catch body
  itself rethrew, the outer `finally` fired three times. The compiler
  now installs a `CatchBarrier` in the pending-finally stack for each
  active `try/catch` handler: throws lowered inside that handler's try
  body pre-run only the finallys they will actually unwind past, while
  `return` / `break` / `continue` continue to run every pending finally
  up to their target. The `compile_rethrow_with_finally` helper that
  double-emitted the finally has been removed in favor of a plain
  rethrow on the catch-escape path. Covered end-to-end by the new
  `conformance/tests/finally_runs_once.*` fixture.

## v0.7.1

### Added

- **Prompt template engine v2.** `render(...)` / `render_prompt(...)` /
  the `template.render` host capability now support `{{ if }} / {{ elif }} /
  {{ else }} / {{ end }}` branching, `{{ for item in xs }} ... {{ else }} ...
  {{ end }}` loops with `{{ loop.index }}`, `.index0`, `.first`, `.last`,
  `.length`, dict iteration (`{{ for k, v in dict }}`), nested path access
  (`{{ user.tags[0] }}`), full boolean and comparison operators in
  conditions, a filter pipeline (`{{ name | upper | default: "anon" }}`)
  with built-in filters (`upper`, `lower`, `title`, `trim`, `capitalize`,
  `length`, `first`, `last`, `reverse`, `join`, `default`, `json`,
  `indent`, `lines`, `escape_md`, `replace`), `{{ include "partial.prompt"
  }}` with optional `with { ... }` scoping and cycle detection,
  `{{# comments #}}`, `{{ raw }} ... {{ endraw }}` verbatim blocks, and
  `{{- trim whitespace -}}` markers. Existing templates remain
  byte-for-byte compatible — pre-v2 `{{ name }}` and `{{ if key }} ...
  {{ end }}` syntax is a strict subset. The duplicate
  `replace()`-based implementation that used to back the
  `("template", "render")` host capability has been removed; host-call
  and script rendering now share the single canonical engine. See
  `docs/src/prompt-templating.md` and
  `docs/src/migrations/template-engine-v2.md`.
- **Preflight template-parse validation.** `harn check` now parses every
  template referenced by a literal `render(...)` or `render_prompt(...)`
  argument and surfaces syntax errors (e.g. unterminated `{{ for }}` block)
  before the pipeline runs.
- **VS Code: `.harn.prompt` / `.prompt` syntax highlighting.** A new
  TextMate grammar ships with the extension.
- **`tool_ref(name)` and `tool_def(name)` stdlib builtins.** Resolve a
  tool-name reference against the currently-bound tool registry, so
  prompt strings and host-bridge code can interpolate canonical tool
  names (and descriptions) instead of hand-typed string literals that
  silently rot on rename. Both builtins throw with the list of
  registered tools when the name is unknown or no registry is bound.
- **`tool_bind(registry)` stdlib builtin.** Installs a tool registry as
  the current thread's active binding, so `tool_ref` / `tool_def` can
  resolve names without plumbing the registry through every call site.
  Pass `nil` to clear the binding. `agent_loop` installs its own tools
  registry automatically for the duration of the run.

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
