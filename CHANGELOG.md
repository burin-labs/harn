# Changelog

Release notes for Harn v0.6 and newer are documented in this file.

Pre-0.6 highlights live in [CHANGELOG-pre-0.6.md](CHANGELOG-pre-0.6.md).
Harn had no external users before 0.6.0, so that archive intentionally keeps
condensed series summaries instead of full per-patch history.

## Unreleased

### Added

- **Connector-safe HTTP policy helpers (#2082).** `std/connectors/shared` now
  exposes `connector_http_request`, `connector_http_json`,
  `connector_http_header`, and `connector_http_rate_limit` so Harn package
  authors can wrap `harness.net.request` with stable error envelopes,
  idempotency-aware unsafe write retries, capped `Retry-After` handling, JSON
  parse categorization, and standard rate-limit header extraction without
  hand-rolling provider loops.
- **`harness.net.*` access policy (#1913).** Adds the `std/net_policy`
  stdlib module so scripts can attach a per-harness allow/deny policy
  to `harness.net.*` requests: `NetPolicy.create({allow, deny,
  default, on_violation})` plus the rule constructors
  `NetPolicy.domain(host)`, `NetPolicy.domain_wildcard(pattern)`,
  `NetPolicy.cidr(range)`, and `NetPolicy.host(host_name, [ports])`.
  `harness.with_net_policy(policy)` returns a new harness whose
  `net.*` calls are gated against the rules. `on_violation` accepts
  `"error"` (throws a typed `NetPolicyViolation`), `"audit_only"`
  (allows the request but records an audit), `"quarantine"` (denies
  and flips the new sticky `harness.is_quarantined()` flag), or a
  user closure `fn(req) -> string` that picks one of the three per
  request. Every evaluation — including the `HARN_NET_POLICY_BYPASS=1`
  short-circuit — emits a `harness.net.policy.audit` event so the
  trust graph keeps an evidence trail. Mock-mode dispatch enforces the
  policy ahead of the canned-response lookup, so conformance fixtures
  can exercise the matcher without touching the network. Tracked
  through the E4.9 row of epic #1765 and lays the runtime groundwork
  for the harn-cloud `workspaces.network_policy` schema.
- **Unified user observability API (#1915).** Adds
  `std/observability` with `obs()` / direct `span`, `log`, `metric`,
  and `event` helpers over the existing VM span machinery. Runtime
  configuration uses named backend handles and factories for
  `Backend.auto`, OTel, Splunk HEC, Honeycomb, pretty stderr, and
  `compose([...])`, with routing rules by `kind`, `level`, or default
  backend. Span attributes merge into emitted log and metric fields,
  `import { obs } from "observability"` is accepted as the short stdlib
  alias, and conformance covers the unified API, backend fan-out, and
  routing payload shapes.
- **`--json` everywhere + `schemaVersion` envelope (#1753).** Extends the
  versioned `JsonEnvelope` contract to the highest-leverage commands that
  previously had no machine-readable mode: `harn lint --json`,
  `harn replay --json`, `harn version --json`, and `harn upgrade --json`
  (pairs naturally with `--check` for a self-update probe). Every new
  envelope is registered in the `harn --json-schemas` catalog with a
  stable `schemaVersion` so agents can dispatch per-command. The full
  contract — envelope shape, error layout, and per-command notes — lives
  at `docs/src/cli-json-contract.md`; a condensed cheatsheet ships in
  `docs/llm/harn-quickref.md`. Stdout stays single-document parseable;
  progress, warnings, and human-readable logs route to stderr.
- **Streaming partial-JSON validation builtin (E5.1, #1773).** New
  `std/json/stream_validate_create`, `stream_validate_chunk`, and
  `stream_validate_finalize` builtins expose the incremental JSON
  validator that already powers `llm_call`'s mid-stream
  `schema_stream_abort` (E5.2) as a standalone user-callable. Each
  call returns a plain-dict verdict
  `{verdict: "pending"|"valid"|"invalid", reason?, path?}` so streaming
  agents (SSE chunks, partial WebSocket frames, custom transports)
  can dispatch on a stable string verdict without pattern-matching enum
  variants. `stream_validate()` returns the same trio bundled as a
  namespace record (`stream_validate.create`/`.chunk`/`.finalize`).
  `finalize` transitions a still-pending validator with a partial
  document to `"invalid"` with
  `reason: "incomplete JSON document at end of stream"`; an already
  `"valid"` or `"invalid"` verdict is returned unchanged. Both the
  closure-bag `stream_validator` API and the new functions share the
  same underlying `JsonStreamValidator` storage, so there is no
  duplicate parser or validator implementation.
- **Experimental MCP file inputs (#1916).** Adds a default-off
  `harn.mcp.configure({experimental: {file_upload: ...}})` opt-in for the
  current draft MCP file-input proposal, SEP-2356. Client code can call
  `harn.mcp.upload_file(client, path, options?)` to encode a local file as an
  RFC 2397 `data:` URI for an `x-mcp-file` schema field, while Harn MCP
  servers can declare those fields with `harn.mcp.file_input(...)`. Both the
  VM MCP server and `harn-serve` MCP adapter validate incoming `data:` URI
  scheme, media-type, and decoded-size constraints before dispatching the tool
  handler. The implementation redacts inline file payloads from replay keys
  and is explicitly tagged as speculative until upstream MCP ratifies the final
  file-input shape.
- **`harn pack` signed content-addressed run bundles (#1779).** Promotes
  `harn pack` to a parent command with a `verify` subcommand and adds
  `--exclude-secrets` to the build path. `harn pack verify <bundle.harnpack>`
  reads a `.harnpack`, recomputes the canonical bundle hash, runs the
  embedded Ed25519 signature check (refusing unsigned bundles without
  `--allow-unsigned`), and cross-checks every per-module
  `source_hash_blake3` / `harnbc_hash_blake3` against the in-archive payload.
  Exits non-zero on any mismatch with stable structured error codes
  (`verify.signature_failed`, `verify.source_mismatch`,
  `verify.bytecode_mismatch`, `verify.recorded_hash_mismatch`,
  `verify.archive_failed`, `verify.unsigned`). `--exclude-secrets` refuses
  to bundle entrypoints whose path matches a conservative secret-bearing
  glob (`.env`, `.env.*`, `*.pem`, `*.key`, `credentials*`, anything under
  `secrets/`); pass `--include-secrets` to be explicit about the historical
  default. Both surfaces emit `JsonEnvelope` payloads under `--json` and
  register with `harn --json-schemas` (`pack verify` schema v1). The
  verifier also accepts `--trust-policy <policy.json>` plus
  `--require-trusted-signer` so compliance pipelines can reject bundles
  signed by unknown or allowlist-mismatched keys with the structured
  `verify.untrusted_signer` error code. The local/registry trust model is
  reused from the existing skill-provenance signer workflow. The
  conformance suite covers happy path, byte-deterministic re-pack, signed
  bundle roundtrip, signed-bundle tamper detection, secrets gate, and the
  JSON envelope contract under `conformance/tests/harn_pack/`.
- **fs / env / random / net `harness.*` sub-handles (E4.4, #1769).**
  The `harness.fs.*`, `harness.env.*`, `harness.random.*`, and
  `harness.net.*` method surfaces are now wired end-to-end in real
  mode. Each sub-handle delegates to the existing ambient builtin
  (and through it, to the existing sandbox path enforcement, egress
  allowlist, transcript tagging, and tape replay machinery), so a
  script that rewrites `read_file("notes.txt")` as
  `harness.fs.read_text("notes.txt")` keeps every guard rail. The
  parser gains four new lint codes (`HARN-LNT-054`..`HARN-LNT-057`)
  paired with four new repair templates
  (`bindings/thread-harness-{fs,env,random,net}`); `harn fix` rewrites
  every call site once `harness` is in scope and points users at the
  surface-changing `bindings/thread-harness` repair otherwise. A new
  `HARN-CAP-201` diagnostic (`sandbox capability denied by active
  sandbox profile`) is attached to runtime errors raised by harness
  sub-handle calls when the active `CapabilityPolicy` rejects the
  path or URL — both the `CategorizedError` (`sandbox violation: ...`)
  shape and the `Thrown(Dict { type: "EgressBlocked", ... })` shape
  pick up the code so scripts can pattern-match on it. `harn-ir`
  attributes `harness.<sub_handle>.<method>` calls back to the
  canonical ambient builtin so `harn graph --json` and the
  routes/invariants pipelines see them as having the same effect
  surface as the legacy ambient call. Conformance fixtures cover the
  null-deny path for `random` (the previously-missing piece in
  `conformance/tests/harness/null_*_denies`), a real-mode
  filesystem roundtrip, deterministic random ranges, and missing-env
  defaults. Actual removal of the ambient `read_file`, `env`,
  `random`, and `http_*` builtins is intentionally deferred to E4.6,
  which migrates the conformance corpus in lockstep.

### Security

- **Template render sandbox hardening.** File-backed `render(...)`,
  `render_prompt(...)`, `render_with_provenance(...)`, `{{ include ... }}`,
  and `host_call("template.render", ...)` now enforce the active
  `workspace_roots` read boundary before reading template files. This closes
  a policy bypass where template rendering could read arbitrary readable files
  even when `read_file(...)` was correctly blocked.

- **Vision OCR sandbox and audit hardening.** Path-backed `vision_ocr(...)`
  inputs now enforce the same active `workspace_roots` read boundary before
  loading image bytes, the Tesseract backend runs through the runtime process
  sandbox, and `audit.vision_ocr` records image metadata and hashes without
  retaining the raw image payload.

- **A2A access-control hardening.** `harn serve a2a` now enforces configured
  API-key/HMAC auth before any non-discovery A2A RPC or REST operation creates,
  reads, cancels, subscribes to, or mutates task state. Unauthenticated callers
  receive HTTP 401 plus `WWW-Authenticate` instead of leaving rejected task
  history behind or reaching task-management and push-callback operations.
  The A2A listener also defaults to loopback; use `--bind 0.0.0.0:PORT`
  explicitly for public deployments behind auth and TLS or a trusted edge.

- **MCP HTTP session teardown hardening.** Script-driven MCP Streamable HTTP
  now applies the same Origin and `mcp-protocol-version` checks to `DELETE
  /mcp` as it already applied to POST/GET, preventing cross-origin session
  teardown when a browser-visible session id is present.

### Changed

- **Stdio now requires the `Harness` capability handle (#1767).** Removes
  the ambient `print`, `println`, `eprint`, `eprintln`, `read_line`, and
  `prompt_user` stdlib builtins in favor of `harness.stdio.*`. `harn fix`
  now plans `bindings/thread-harness` when an existing harness binding can be
  threaded through local helper calls, and downgrades to the
  `bindings/thread-harness-needs-param` surface-changing repair when a new
  `harness: Harness` parameter is required.

- **Linear typecheck scaling on large files (#2093).** Restructures
  `TypeScope.parent` from `Box<TypeScope>` (deep-cloned on every scope
  entry) to `Rc<TypeScope>` so child scope creation is an `Rc::clone`
  rather than a recursive copy of the parent chain. Each function /
  pipeline body now enters its scope via a refcount bump against the
  shared root, and non-generic call sites skip the per-call
  `call_scope.clone()` by borrowing the caller's scope directly.
  Cold-start typecheck phase on a synthetic file with one-line fns and
  500 call sites in the main pipeline drops from previously
  super-linear to flat O(n): 500 fns 69 ms → 3 ms (23×), 2000 fns
  438 ms → 6 ms (73×), 5000 fns 2.18 s → 14 ms (156×), 10000 fns
  8.27 s → 29 ms (285×). LSP re-typecheck on save and `harn check` in
  pre-commit on large projects pick up the same speedup.

## v0.8.27

### Added

- **OpenTrustGraph schema v0.1 (#1778).** Bumps the trust-record schema
  discriminator from `opentrustgraph/v0` to `opentrustgraph/v0.1` and
  reserves three additional keys under `TrustRecord.metadata`:
  `effects_grant` (typed `EffectRecord` list extended by the parent),
  `effects_used` (typed `EffectRecord` list the action actually
  exercised), and `parent_record_id` (pointer at the parent record's
  `record_id`). Verifiers can now prove that a child agent's
  `effects_used ⊆ parent.effects_grant`, closing the receipt-chain
  inclusion proof flagged in E5.5. Backwards compatible at the record
  layer: `TrustRecord` deserialization still accepts the older
  `opentrustgraph/v0` discriminator for one patch release window per
  the new [`opentrustgraph-spec/CONFORMANCE.md` §5.1][otg-v0-1] before
  v0 is retired. The chain hash inputs and chain-export envelope
  (`opentrustgraph-chain/v0`) are unchanged because metadata was
  already hash-covered. Ships the new `v0.1` JSON Schema at
  `opentrustgraph-spec/schemas/trust-record.v0.1.schema.json`, a new
  3-agent fixture
  `opentrustgraph-spec/fixtures/valid/effect-inheritance-chain.json`
  demonstrating the inheritance invariant end-to-end, and extends the
  pure-Python reference verifier (`examples/python/verify_chain.py`) to
  enforce the subset check. New helpers on `TrustRecord`
  (`with_effects_grant`, `with_effects_used`, `with_parent_record_id`,
  plus matching getters and setters) keep callers from poking at the
  reserved metadata keys by hand. Exposes new public constants
  (`OPENTRUSTGRAPH_SCHEMA_V0_1`, `OPENTRUSTGRAPH_ACCEPTED_SCHEMAS`,
  `METADATA_KEY_EFFECTS_GRANT`, `METADATA_KEY_EFFECTS_USED`,
  `METADATA_KEY_PARENT_RECORD_ID`) and the matching re-exports from
  `harn_vm`. The harn-cloud receipt-store replatform is tracked
  separately (paired ticket under epic #1773); both versions are
  accepted on ingest until then.

  [otg-v0-1]: https://github.com/burin-labs/harn/blob/main/opentrustgraph-spec/CONFORMANCE.md#51-v01-reserved-metadata-keys-1778

- **Suspend/resume conformance suite (S-11, #1847).** Closes the
  remaining gap in the suspend/resume executable spec (epic #1836) by
  adding four new fixtures under `conformance/tests/agents/` to pair
  with the eight already shipped. `suspend_self_park_open.harn` pins
  the open-park contract: `agent_await_resumption(reason)` with no
  `conditions` reports `initiator: "self_initiated"`,
  `suspension.conditions == nil`, and `auto_resume_trigger == nil`,
  and only an explicit `resume_agent(handle)` wakes the worker.
  `suspend_self_park_with_input.harn` asserts the resume-continuity
  reminder embeds the verbatim resume input string and the suspend
  reason, that the reminder is present on the resumed turn's system
  prompt, and that it is consumed within one turn (the turn after the
  resumed one no longer sees it). `suspend_conditioned_trigger.harn`
  pins the `conditions.trigger` auto-resume contract: the suspension
  carries a non-nil `auto_resume_trigger` handle, `trigger_fire(...)`
  reports `dispatched`, and the loop drives itself to `completed`
  without any explicit `resume_agent` call from the pipeline.
  `suspend_cold_restart.harn` pins the cross-process `harn run --resume
  <snapshot>` contract: a top-level `agent_loop(...)` self-park writes
  a non-empty snapshot file, the process exits with
  `status=suspended`, and a second `harn run --resume <snapshot>
  --json` invocation emits a `done` event with
  `value.status=completed` and `value.has_transcript=true`. The other
  eight fixtures from the S-11 ticket
  (`suspend_midloop_basic`, `suspend_nested`,
  `suspend_continue_transcript_false`, `suspend_close_while_suspended`,
  `suspend_double_resume_race`, `suspend_timeout_resume_with_summary`,
  `suspend_timeout_fail`, `suspend_daemon_step_parity`) were already in
  place; the new four bring the suite to the full 12-fixture contract
  table in the issue. All fixtures are deterministic — they use
  `llm_mock` / `mock_time` / `trigger_fire`, no wall-clock sleeps —
  and ride the existing `cargo run --bin harn -- test conformance
  --filter suspend_` runner.

- **Effect inheritance enforcement (E5.4, #1777).** Adds the dispatcher-side
  ⊆ check for typed handoff effects against the parent's declared effect
  set. New diagnostic code `HARN-CAP-301`
  (`Code::EffectInheritanceViolation`) surfaces from `harn check`'s
  preflight pass when a `spawn_agent({...})` call inside a parent
  function/pipeline body grants effects the parent does not declare;
  parent and child sets are derived from the same `compute_handoff_effects`
  analyzer the runtime guard uses. New runtime guard
  `enforce_spawn_handoff_effects(handoff, parent_effects)` in
  `crates/harn-vm/src/orchestration/handoffs.rs` returns a typed
  `EffectInheritanceViolation` payload (with stable `_type`,
  `handoff_id`, `source_persona`, `target_label`, `violations`,
  `diagnostic_code`, `repair_id`, `repair_safety`, `message` fields) that
  the dispatcher emits as an `EffectInheritanceViolation` deny event;
  `report_effect_inheritance_violation(...)` emits the matching
  structured log under the `policy.effect_inheritance` category. New
  shared library helpers in `crates/harn-vm/src/orchestration/policy/effects.rs`:
  `effect_subset_violations(parent, child)` (the core ⊆ check),
  `effect_kind_label(...)` and `effect_record_summary(...)` (used by
  both diagnostic messages and deny payloads). Both static and runtime
  paths suggest the same repair (`policy/narrow-child-effects`,
  `safety: surface-changing`). Coverage: 16 new policy/effect unit
  tests, 4 new preflight integration tests in
  `crates/harn-cli/src/commands/check/tests.rs`, and two conformance
  fixtures (`conformance/tests/agents/effects_inheritance_subset_allowed.harn`,
  `…/effects_inheritance_empty_child.harn`) exercising the subset-allowed
  and empty-child paths.
- **Suspend/resume docs (S-13, #1849).** Adds the user-facing
  reference for the agent suspend/resume primitive (epic #1836). New
  mdBook page `docs/src/agent-lifecycle.md` covers the lifecycle
  builtins (`suspend_agent`, `resume_agent`,
  `parse_resume_conditions`, `agent_await_resumption`,
  `agent_lifecycle_tools`), the `ResumeConditions` shape (trigger /
  timeout / on_event), self-park mid-loop, parent-driven
  pause/resume via `subagent_pause` / `subagent_resume`, conditioned
  resume with trigger + timeout, the four `ResumeBy.*` callbacks and
  the `default_resume_by(...)` policy, transcript continuity (the
  single-shot `resume_continuity` system reminder vs
  `continue_transcript: false`), top-level loop cold-restore via
  `harn run --resume <snapshot_path>`, daemon idle as a degenerate
  case, the `HARN-SUS-*` diagnostic codes, and the cooperative-pause
  gotchas. Replaces the terse "Agent lifecycle tools" subsection in
  `docs/llm/harn-quickref.md` with an expanded "Agent lifecycle:
  pause, resume, self-park" section that lists the three model-facing
  tools, ships three executable snippets (self-park, parent-driven,
  conditioned), summarizes resume responsibility and `ResumeBy.*`,
  documents transcript continuity, and surfaces the gotchas.
  Adds an "Agent lifecycle (suspend/resume)" section to
  `spec/HARN_SPEC.md` with the formal state machine
  (`running -> suspended -> running / closed -> done`), the
  `Suspension` / `ResumeConditions` /
  `AgentAwaitResumptionRequest` shapes, the four canonical
  `ResumeBy.*` presets with `default_resume_by(...)` decision tree,
  the `PreSuspend` / `PostSuspend` / `PreResume` / `PostResume` hook
  contract, the top-level cold-restore contract, the daemon-idle
  reduction, and the cooperative cancellation contract. Wires the
  new mdBook page into `docs/src/SUMMARY.md` under Agent runtime
  next to Agent state.
- **Pipeline lifecycle docs (P-10, #1863).** Documents the callback-first
  pipeline lifecycle (epic #1853). New mdBook page
  `docs/src/pipeline-lifecycle.md` covers the lifecycle event order
  (`PreFinish` → `on_finish` → `OnUnsettledDetected` → `PostFinish`),
  the harness read/write surface (`unsettled_state` plus the 11 drain
  methods), the four `OnFinish` presets (`on_finish_abandon`,
  `on_finish_drain`, `on_finish_block_until_settled`,
  `on_finish_handoff_to`), the six composable combinators (`compose`,
  `first_available`, `with_telemetry`, `with_timeout`, `if_unsettled`,
  `when`), the three `OnBudget` strategies, the full hook-event tables
  for the finish / suspend-resume / drain gates, and a Design rationale
  section that explains the callback-first vs enum-dispatch decision
  with cross-refs to Temporal `wait_condition` and Restate sagas.
  New `docs/src/cookbooks/lifecycle.md` ships five end-to-end recipes:
  nightly settlement handoff with `on_finish_handoff_to`, custom audit
  per drain decision via an `on_drain_decision` session hook +
  `with_telemetry(on_finish_drain)`, long-paused agent with a
  `post_resume` operator telemetry hook, business-hours `pre_suspend`
  deny chain with a paired 1-turn reminder, and a replay-deterministic
  multi-suspend test harness using `mock_time` /
  `flush_trigger_aggregations` / `pipeline_lifecycle_audit_log_take`.
  Adds a "Pipeline lifecycle: drain, on_finish, composable handlers"
  section to `docs/llm/harn-quickref.md` with preset / combinator /
  budget / hook-event tables, three common-pattern snippets (nightly
  handoff, custom audit, clean abort), and cross-refs to the
  suspend/resume primitive (harn#1836). Adds a Pipeline lifecycle
  subsection to `spec/HARN_SPEC.md` with formal definitions of the
  `Pipeline.on_finish` semantic, the `Harness` type, the `DrainAgent`
  constrained tool surface + `HARN-DRN-001` ordering enforcement, the
  40-event lifecycle event taxonomy, and the four replay-determinism
  rules (cached resume input, memoized drain decisions, signed
  timestamps, one-shot registration). Adds a "Pipeline lifecycle" +
  "Lifecycle cookbook" entry pair to `docs/src/SUMMARY.md` and
  re-syncs `docs/src/language-spec.md` via `make sync-language-spec`.
  Closes the docs gap for the lifecycle epic; user-facing GA is now
  unblocked.
- **Tool hooks docs + harn-canon contribution guide (TH-08, #1901).**
  Adds the user-facing reference, cookbook, and contributing guide for
  the `preset_run_command` wrapper shipped under epic #1884. New
  mdBook page `docs/src/tool-hooks.md` covers the full
  `preset_run_command(config)` surface: the `stacks` / `registry` /
  `custom_rules` / `mode` / `inner` / `llm_classifier` keys, the
  decision-envelope shape, the three shipped modes
  (`tool_hooks_mode_rewrite_with_audit` / `_deny_with_explanation` /
  `_passthrough_only_audit`), custom-mode authoring with
  `tool_hooks_emit_audit` and `tool_hooks_inject_reminder`, the
  `ToolRule` / `Catalogue` / `Registry` schemas with field-level
  tables, the seven shipped catalogues (`harn-canon/universal`,
  `/rust`, `/python`, `/typescript`, `/swift`, `/sql`, `/harn`) with
  their rule rosters, the LLM classifier opt-in (verdict shape,
  threshold dispatch, cache TTL, transport-error degradation, trust
  contract), the lifecycle audit kinds (`tool_rewrite`, `tool_denied`,
  `tool_rule_warning`, `tool_hook_classifier_verdict`,
  `tool_hooks.reminder_injected`), and composition with the broader
  `register_tool_hook` PreToolUse / PostToolUse surface. New
  `docs/src/cookbooks/tool-hooks.md` ships ten recipes: per-stack
  agents (rust, python, typescript, swift, sql, harn), a polyglot
  agent with classifier fallback, audit-only rollout, no-`inner`
  preview, and composition with `register_tool_hook`. New
  `docs/src/contributing/preset-hooks.md` documents the harness-local
  → per-package → harn-canon promotion ladder, the harn-canon bar
  (stack-canonical / universally beneficial / cheap / stable /
  recoverable), the rule-add / new-stack workflows, the local gate
  checklist (`make conformance`, `make lint-md`,
  `make check-docs-snippets`, `mdbook build docs`), the catalogue
  authoring gotchas (no regex lookaround, leading-command anchor,
  prefix-extension overlap, `env_or` escape hatches, idempotent
  rewrites, no `now()`/`shell()` in predicates), and a reviewer
  checklist. Expands the "Catalogue-driven `run_command` hooks"
  section of `docs/llm/harn-quickref.md` with the catalogue auto-seed
  contract, the full classifier opt-in, and cross-references to the
  new pages; replaces the stale TH-05-is-reserved sentence with the
  shipped classifier docs. Adds a "Preset `run_command` tool hooks"
  subsection to `spec/HARN_SPEC.md` (formal `ToolRule` / `Catalogue`
  field tables, `preset_run_command` config keys, the three shipped
  modes' side effects, the classifier verdict shape, and the uniform
  decision-envelope schema). Wires the new pages into
  `docs/src/SUMMARY.md` under "Hooks (tool, persona, session
  lifecycle)" so the reference, cookbook, and contributing guide
  appear together.
- **Pool docs + cookbook (PL-08, #1893).** Adds the user-facing
  reference + cookbook for agent pools (epic #1883). New mdBook page
  `docs/src/agent-pools.md` covers pool creation (`pool_create`
  options, the handle shape), submit semantics (priority, fairness
  key, idempotency, task-handle shape), the four queue strategies
  (`fifo`, `priority`, `lifo`, `fair_round_robin`), backpressure
  policies (`backpressure_queue` with `block_submitter` /
  `drop_oldest` / `drop_newest` / `fail_submitter`, `fail_fast`,
  `ring_buffer`), the four durability scopes (`session`, `pipeline`,
  `tenant`, `org`) with the pipeline reload + `stale_after_ms`
  contract, the `SpawnToPool` trigger handler, inspection surface
  (`pool.size`, `pool.snapshot`, `pool_get`, `pool_list`,
  `harness.unsettled_state().pool_pending_tasks`), the
  `lifecycle.pool.audit` topic + OTel spans, the `HARN-POL-*`
  diagnostic codes, a pick-the-right-primitive table
  (`parallel each` vs pool vs scope tiers), and a SOTA comparison
  table (Temporal, Inngest, Restate) explaining the design rationale.
  New `docs/src/cookbooks/pools.md` ships four end-to-end recipes:
  rate-limited webhook processor with per-source fair queue +
  ring-buffer overflow, GPU-routed inference pool sized to the GPU
  count with `scope: "tenant"` for harn-cloud worker-tier routing,
  cross-customer fairness with 20-wide pool +
  `fair_round_robin("tenant_id")`, and a burst absorber for nightly
  batch jobs with a 50000-deep queue + idempotency keyed by
  `(date, customer)`. Expands the "Agent pools" section in
  `docs/llm/harn-quickref.md` with the full submit-option set,
  queue-strategy + backpressure tables, scope tiers, the
  `SpawnToPool` snippet, and cross-references. Adds an "Agent pools"
  subsection to `spec/HARN_SPEC.md` (with formal `PoolHandle`,
  `PoolTaskHandle`, `QueueStrategy`, `Backpressure`, `PoolScope`, and
  `SpawnToPoolHandler` shapes) plus the `HARN-POL-*` block in the
  diagnostic appendix. Wires the new pages into `docs/src/SUMMARY.md`
  under Orchestration next to Agent channels.
- **`harness.system.*` host introspection (E4.8, #1912).** Adds a seventh
  capability slice under the `Harness` handle that lets scripts read
  cross-platform host metadata without ambient OS access. New methods on
  `harness.system`: `cpu()` returns `{count, physical_count, model,
  frequency_mhz, usage_pct}`; `memory()` returns
  `{total_bytes, used_bytes, available_bytes, total_gb, used_gb,
  available_gb, pressure}` with `pressure` bucketed to
  `low|medium|high|unknown`; `gpus()` returns a (currently empty) list
  so scripts can write `if !gpus.is_empty()` portably while richer
  NVML / Metal / OpenCL detection lands as a follow-up; `temperature()`
  returns `{components: [{label, celsius, max_celsius,
  critical_celsius}, ...]}` with an empty list on hosts whose `sysinfo`
  backend doesn't expose thermal sensors (notably Apple Silicon and
  most containers); `platform()` returns `{os, arch, version, kernel,
  long_os_version, hostname}`; `processes()` returns a list of
  `{pid, name, ...}` entries with the current Harn process always
  included and tagged `is_self: true` / `is_harn_owned: true`, plus
  `parent_pid`, `cpu_pct`, `mem_bytes` on harn-owned entries. Peer
  processes appear as bare `{pid, name, is_harn_owned: false}` entries
  — we deliberately do **not** leak `command_line`, `environ`, or
  `cwd` for other processes to avoid exfiltrating credentials and
  prompts from peer agents. New `harness_system::register_harn_owned_pid`
  / `unregister_harn_owned_pid` helpers let subprocess spawners tag
  detached children so they keep their harn-owned status even after
  the parent->child link is broken. The surface is gated by the harness
  handle just like the other sub-handles: `Harness::null()` denies
  every call and records a `DenyEvent`; `Harness::mock()` returns
  deterministic synthetic snapshots so conformance fixtures can
  exercise the shape without observing the real host. Wires
  `HarnessKind::System` into the typechecker (`harness.system` maps to
  `HarnessSystem`), the effect-policy analyzer (system reads produce
  no `EffectRecord` since they're pure host reads gated by the
  capability handle), the property-access error message, and the
  null-deny test fixture. New backing crate dependency: `sysinfo`
  (default-features-off). Conformance: `harness/system_basic`,
  `harness/system_gpus_temperature`, `harness/system_processes`, and
  `harness/null_system_denies`.
- **Suspend/resume protocol contribution RFCs (S-12, #1848).** Authors
  two new upstream-proposal documents under
  `docs/src/protocol-contributions/`: ACP `session/suspend` +
  `session/await_resumption` (the suspend-side companion to the
  already-shipped `session/resume`, [ACP #1726][acp-1726]) and A2A
  explicit `TaskState.PAUSED_BY_CLIENT` / `PAUSED_BY_AGENT` with
  matching `tasks/pause` / `tasks/await_resumption` / `tasks/resume`
  JSON-RPC methods. Each RFC describes the proposed wire format,
  migration path from our existing `_meta.harn.suspend` /
  `metadata.harn.pause` envelopes, reference-impl status against the
  shipped `__host_worker_suspend` + `WorkerSuspension` primitive,
  and open questions for upstream maintainers. Both RFCs deliberately
  share field names (`handle`, `reason`, `conditions`, `cause`,
  `continueTranscript`) so cross-protocol bridges can round-trip
  pauses verbatim. Wires the two new RFCs into
  `docs/src/SUMMARY.md` and the
  `docs/src/protocol-contributions/README.md` index (now grouped by
  RFC family: reminder injection under #1829, suspend/resume under
  #1848). Upstream filings remain a maintainer action — see #1848 for
  the outstanding work.

[acp-1726]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1726

- **Channels docs + cookbook (CH-09, #1880).** Adds the user-facing
  reference + cookbook for durable agent channels (epic #1870). New
  mdBook page `docs/src/agent-channels.md` covers the emit surface
  (scopes, idempotency, signed timestamps), the `channel.emit`
  trigger source, the `batch { count, window, key, expire_action }`
  aggregation primitive, the `ReminderInject` handler variant, the
  guardrails middleware layer (CH-11), three observability topics,
  the `HARN-CHN-*` diagnostic codes, and a SOTA comparison table
  (Inngest, Temporal, Restate, A2A) explaining the design rationale.
  New `docs/src/cookbooks/channels.md` ships five end-to-end recipes:
  release handshake (PR agent → batched release agent →
  merge-captain subscribers), periodic check-in via batched tool-call
  counting + `ReminderInject`, multi-agent feedback loop (planner +
  reviewers), tenant-scoped pipeline progress dashboard, and
  cross-pipeline coordination via drain handoff. Extends the "Durable
  agent channels" section in `docs/llm/harn-quickref.md` with the
  trigger + batch + ReminderInject surface, a four-row pick-the-right-
  primitive comparison table (handoffs vs triggers vs suspend/resume
  vs channels vs channels+batch+reminder), the `HARN-CHN-*` and
  `HARN-REP-CHN-*` code lists, and a guardrails one-liner. Adds a
  "Durable agent channels" subsection to `spec/HARN_SPEC.md` (with
  formal `ChannelScope`, `BatchFilter`, `ReminderInjectHandler`
  shapes) plus the `HARN-CHN-*` block in the diagnostic appendix.
  Wires the new pages into `docs/src/SUMMARY.md` under Orchestration.
- **`needs-human` autonomy class (T8, #1792).** Formalizes `needs-human` as
  a first-class, transverse autonomy discipline on top of the existing
  `AutonomyTier` (`Shadow` / `Suggest` / `ActWithApproval` / `ActAuto`).
  Any operation tagged `needs-human` is now denied with the structured
  `HARN-AUT-NEEDS-HUMAN` deny code regardless of the resolved tier —
  even an `ActAuto`-tier caller cannot auto-apply a `needs-human`
  side effect. `AutonomyPolicy` grows three additive fields for tagging:
  `requires_human: bool` (blanket), `requires_human_agents: BTreeSet<String>`
  (per-agent), and `requires_human_actions: BTreeSet<String>` (per-builtin
  or per-action-class, e.g. `"write_file"` or `"fs.write"`). The dispatcher
  emits a non-blocking approval-request with `detail.autonomy_class =
  "needs-human"`, `detail.requires_human = true`, and `detail.deny_code =
  "HARN-AUT-NEEDS-HUMAN"` so approval surfaces (Slack-approval, IDE,
  portal, `hitl_pending`) can render the row distinctly from a normal
  tier-driven approval ask. The corresponding `TrustRecord` is appended
  with `outcome = "denied"`, `metadata.autonomy_class = "needs-human"`,
  `metadata.requires_human = true`, `metadata.deny_code =
  "HARN-AUT-NEEDS-HUMAN"`, and `metadata["autonomy.enforcement"] =
  "needs_human_denied"`. The autonomy-class string mirrors
  `RepairSafety::NeedsHuman.as_str()` from `harn-parser` so the
  autonomy-surface and the repair-safety surface stay in lockstep with
  E1.2 (#1747). Conformance: `conformance/tests/autonomy/autonomy_needs_human_basic.harn`
  (per-action tag denies under `act_auto`, approval-request and trust
  record carry the tag) and `autonomy_needs_human_blanket.harn` (blanket
  and per-agent shapes).
- **OAuth docs + provider cookbook (OA-08, #1909).** Adds
  `docs/src/oauth.md` as the user-facing reference for the full OAuth
  stack (`std/oauth/{providers,storage,client,device_flow,dynamic_registration,redaction}`),
  with per-provider cookbook recipes for GitHub, Slack, Linear, Notion,
  Google, Microsoft, Atlassian, Discord, GitLab (cloud + self-hosted),
  and Bitbucket, plus cross-cutting recipes for headless CI agents
  (device flow), org-shared bots (`harn_cloud_org` storage), and
  custom enterprise OIDC providers. Each recipe includes a
  provider-specific gotcha (Slack single-use refresh, Linear
  comma-separated scopes + GraphQL userinfo, Notion page-picker scoping,
  Google offline_access + prompt=consent, Microsoft Graph delegated
  scopes vs OIDC claims, Atlassian rotating refresh + cloudid lookup,
  Discord bot-vs-user token split, GitLab dual-rotation, Bitbucket
  workspace + client-credentials guidance). Adds an `Authentication`
  pointer + dedicated device-flow / dynamic-registration / redaction
  subsections to `docs/llm/harn-quickref.md`, an OAuth subsection
  (including the `HARN-OAU-*` diagnostic code appendix) to
  `spec/HARN_SPEC.md`, and wires the new page into `docs/src/SUMMARY.md`.
- **Channel guardrails middleware (CH-11, #1911).** Inter-agent
  `emit_channel(...)` calls now run through a pluggable middleware
  layer before the durable journal append. Each guardrail returns one
  of three verdicts — `allow` (silent passthrough), `warn` (emit
  proceeds and a `channel_guardrail_warning` lifecycle audit is
  recorded), or `block` (emit is dropped, the caller receives a
  synthetic `{blocked: true, block_reason, guardrail_fired}` receipt,
  and a `channel_guardrail_blocked` audit is persisted to the
  `lifecycle.channel.audit` topic AND the in-process lifecycle audit
  log so the block itself is durable + observable). Worst verdict
  across every registered guardrail wins; a `block` short-circuits
  remaining guardrails so they cannot suggest the emit went through.
  New Rust builtins: `channel_guardrail_register(config_dict) ->
  id_string`, `channel_guardrail_unregister(id) -> bool`,
  `channel_guardrail_list() -> [string]`, `channel_guardrail_clear()`.
  The registry is thread-local so peer pipelines on other threads
  cannot poison each other; `reset_channel_state()` clears the
  registry alongside the per-thread channel log. Two scanner kinds
  ship in v1: `prompt_injection_signature` (heuristic regex sweep
  over every string in the payload tree — recursively walks dicts /
  lists so nested adversarial text cannot hide; eleven seed patterns
  cover instruction-override, system-prompt spoof, role redefinition,
  data-exfil request, and jailbreak-banner families; the audit
  records a coarse family label rather than the matched regex so the
  attack fingerprint does not leak back to the producer) and
  `custom` (user-supplied Harn closure returning `nil` /
  `"allow"|"warn"|"block"` / `{verdict, reason?}`; evaluated via the
  async-builtin child VM, so closure-bound side effects work the
  same as in `tool_hooks_match`). A new `std/channel_guardrails`
  module exposes ergonomic presets: `prompt_injection_scanner(opts)`,
  `llm_risk_classifier(opts)` (mirrors the TH-05 #2017 pattern with a
  JSON-verdict meta-prompt, sub-threshold-degrades-to-warn safety
  net, and transport-error-degrades-to-allow so a flaky model cannot
  DoS the channel), and `register_guardrail(opts)` for raw custom
  closures. Conformance: four new fixtures under
  `conformance/tests/triggers/trigger_channel_guardrail_*` cover the
  signature scanner blocking adversarial payloads while letting safe
  ones through, custom-closure verdict honoring across all three
  shapes, LLM classifier mock with confident-block + sub-threshold +
  explicit-allow, and the allow-passthrough silence contract +
  register/list/unregister surface. Rust-side coverage adds eight
  unit tests exercising nested-string walking, registration replace-
  on-duplicate, applies_to filtering, and short-circuit-on-block.
- **Bounded sandboxed const-evaluator (T7, #1791).** New `const NAME [: Type] = EXPR`
  binding form runs its initializer through a strict compile-time
  evaluator in `crates/harn-parser/src/const_eval/`. Folding is
  allowlist-based: literal arithmetic, string concatenation, literal
  lists/dicts, ternary / if-else, subscript access, and a small set of
  pure stdlib builtins (`len`, `format`, `min`, `max`, `abs`, `floor`,
  `ceil`, `round`, `lowercase`, `uppercase`, `trim`, `concat`, `join`)
  are accepted. Step (`MAX_STEPS = 100_000`) and recursion
  (`MAX_DEPTH = 256`) caps are checked on every reduction, never
  amortized. Sandbox violations (`harness.*`, fs / net / env / process /
  spawn / parallel / select / try / yield / emit, user functions,
  mutation, loops) are rejected with new diagnostic codes:
  `HARN-MET-001` (disallowed expression shape), `HARN-CST-001` (step
  budget), `HARN-CST-002` (recursion depth), `HARN-CST-003` (sandbox
  violation), `HARN-CST-004` (value-level runtime error during folding).
  At runtime the binding lowers to a `let`-equivalent — the same
  expression is re-evaluated by the VM and byte-equality with the
  compile-time fold is guaranteed by construction (pure subset).
  Pre-positions compile-time prompt template specialization, schema
  derivation from types, and `harn graph --json` static facts; no
  immediate consumer in this release.
- **DAP subagent threads + suspend/resume integration (S-17, #1868).**
  `harn-dap` now bridges spawned subagents onto DAP's `Thread`/`stopped`/
  `continued` event model. Each `agent_loop` / `sub_agent_run` worker
  becomes its own DAP thread (ids start at 100 to keep them visually
  distinct from `main`=1 and ACP-session threads in the 2.. range);
  `parent_worker_id` propagates onto a `parentId` extension so IDEs can
  render lineage trees; `last_status` / `suspend_reason` ride alongside
  on the `threads` response. `WorkerSuspended` raises a per-thread
  `stopped { reason: "suspend", description: <suspension reason>,
  allThreadsStopped: false }` so the suspended subagent shows as paused
  without freezing the user's debug session, and `WorkerResumed` emits
  a matching `continued`. `WorkerWaitingForInput` maps to
  `stopped(reason: "pause")`, `WorkerCompleted`/`WorkerCancelled` emit
  `thread { reason: "exited" }`, `WorkerFailed` emits both a
  `stopped(exception)` and an exit, and `WorkerProgressed` is silent so
  the IDE doesn't flood with no-op churn. The `initialize`
  capabilities advertise three new `exceptionBreakpointFilters`:
  `break-on-suspend`, `break-on-resume` (forces a main-thread pause so
  the user can step through the resume continuation), and
  `break-on-drain-decision` (registered for issue spec parity; drain
  decisions today flow through the lifecycle-hook path rather than
  `AgentEvent`, so the filter currently surfaces ambient events
  without forcing a stop — documented as a known limitation).
  Privacy: only worker id, name, mode, status, parent id, and the
  short human-readable suspend reason flow through DAP — transcripts,
  conditions, and arbitrary metadata blobs are deliberately filtered
  per issue #1867. New module `crates/harn-dap/src/debugger/subagents.rs`
  owns the `SubagentTracker` (queue + `worker_id` → DAP-thread map)
  and the `SubagentEventSink` that captures `AgentEvent::WorkerUpdate`
  observations between VM steps; the registration drops with the
  debugger so a session never leaks a wildcard sink. Powers the
  burin-code#940 (B-06) consumer side. Coverage: nine new unit tests
  exercising filter registration, drain → DAP event mapping, suspend
  description propagation, `break-on-resume` pause arming, parent
  lineage in `handle_threads`, `WorkerFailed` two-event sequence,
  `WorkerProgressed` silence, and direct sink-to-tracker handoff.
- **Cross-session wildcard `AgentEventSink` (#1868 support).**
  New `harn_vm::agent_events::register_wildcard_sink` / `unregister_wildcard_sink`
  / `WildcardSinkHandle` API lets a process-global observer subscribe
  to every emitted `AgentEvent` regardless of `session_id`. Wildcards
  fan out *after* the session-scoped registry so per-session ordering
  stays intact for existing consumers. Test-only thread-owner filtering
  mirrors the per-session sink registry so parallel tests don't
  cross-pollute each other. The DAP debugger is the first consumer
  (issue #1868); other cross-session observers (the upcoming portal
  live view, audit ledger sinks) can drop in without further plumbing.
- **Settlement-agent drain loop (P-03, #1856).** Lands the real
  implementation behind `harness.spawn_settlement_agent` that
  `on_finish_drain` (P-02) and the `PreDrain` / `PostDrain` / `OnDrainDecision`
  hook seam (P-06) were already wired against. The loop is a bounded,
  deterministic walk over the unsettled-state snapshot in the documented
  order — suspended subagents → queued triggers → partial handoffs →
  in-flight LLM calls → pool pending — applying a default disposition
  per item (cancel suspended subagents, acknowledge stale triggers,
  acknowledge partial handoffs as `deferred`, drain in-flight LLM calls,
  defer pool tasks). Each disposition records a `drain_decision`
  lifecycle audit and fires the `OnDrainDecision` hook chain (Allow /
  Block / Modify), so VM-side hooks observe the disposition before it
  persists. The loop terminates when the snapshot drains or when the
  per-call budget (default 5, configurable via the third arg to
  `spawn_settlement_agent`, hard-capped at 20) is exhausted; on
  exhaustion a `drain_unsettled_remaining` audit captures the remainder.
  HARN-DRN-001 ordering enforcement now ships alongside the loop:
  `harness.acknowledge_trigger` and `harness.acknowledge_handoff` reject
  out-of-order calls with the documented `HARN-DRN-001` reason so a
  caller (or a future LLM-driven settlement variant) cannot finalize a
  later category while earlier work is still pending. A new
  `__host_settlement_agent_active()` builtin exposes the
  constrained-surface flag so conformance fixtures and IDE hosts can
  observe when the loop is in scope. Five new conformance fixtures pin
  the contract: `pipeline_drain_settlement_audits` (per-item
  `drain_decision` audits + terminal `pipeline_finalized`),
  `pipeline_drain_settlement_ordering` (FIFO bucket processing),
  `pipeline_drain_settlement_max_iterations` (budget enforcement + the
  `drain_unsettled_remaining` audit), and
  `pipeline_drain_settlement_constrained_surface` (active flag scoped
  to the loop body). The previously-`@xfail`ed
  `pipeline_drain_ordering_enforcement` fixture now gates the real
  HARN-DRN-001 contract (xfail threshold drops from 1 to 0). Part of
  epic #1853 (declarative agent lifecycle).
- **Tool-hook conformance suite (TH-07, #1900).** Adds nine focused
  `conformance/tests/stdlib/preset_hooks_*` fixtures that gap-fill the
  TH-* surface against the spec's named scenarios:
  `preset_hooks_basic_rewrite`, `preset_hooks_deny_mode`,
  `preset_hooks_passthrough_audit`, `preset_hooks_no_match_passthrough`,
  `preset_hooks_catalogue_composition`,
  `preset_hooks_custom_rule_priority`,
  `preset_hooks_per_stack_filtering`,
  `preset_hooks_llm_classifier_confidence`, and
  `preset_hooks_universal_rules_always_apply`. Each fixture isolates a
  single contract (mode dispatch, rule-priority resolution across
  catalogues, custom-rule precedence, stack filtering, LLM-classifier
  threshold gating, universal-rule stack independence) so a regression
  in any one surface points at the right TH-0X fix rather than a
  broad-coverage test. The pre-existing
  `tool_hooks_catalogue_{rust,python,typescript,swift,sql,harn,universal}`
  fixtures already satisfy the spec's "two real-command fixtures per
  catalogue" rider with multi-probe coverage of every shipped rule.
  Combined suite runtime stays well under the 40-second budget
  (~4.5s for the 9 new fixtures; ~25 fixtures total in the
  tool/preset hook namespace). Part of epic #1884 (Preset tool hooks
  library).
- **Channel conformance suite (CH-08, #1879).** Closes the channels
  primitive's executable-spec gap with five new paired fixtures that
  fill out the CH-01..CH-07 coverage map: `channel_emit_idempotent`
  (duplicate `emit_channel` with the same explicit `id` is a no-op for
  trigger fan-out — first emit fires the handler exactly once, second
  returns `duplicate=true`), `channel_scope_pipeline_siblings`
  (`scope: "pipeline"` with an explicit `pipeline_id` routes through a
  shared topic so sibling readers see each other's emits; distinct
  pipeline ids stay isolated), `trigger_channel_reminder_inject_current`
  (`ReminderInject` with `target: "current"` gracefully drops with a
  `target_missing` audit when no agent session is on the current-session
  stack — the documented failure-mode contract), `trigger_channel_reminder_inject_batched`
  (a batched channel trigger paired with `ReminderInject` lands exactly
  one `SystemReminder` per batch dispatch, not per constituent event,
  and the batch counter resets cleanly after a fire), and
  `trigger_channel_replay_batch_determinism` (constituent
  `payload_hash` values match byte-for-byte across "first" and
  "replay" walls of identical batched emits, while distinct payloads
  inside a single batch keep distinct hashes so the replay oracle can
  pinpoint which constituent drifted). All fixtures use the deterministic
  `mock_time(...)` / `advance_time(...)` / `flush_trigger_aggregations()`
  pattern — no wall-clock sleeps. Brings the `cargo run --bin harn --
  test conformance --filter channel` count from 23 to 28.
- **Suspend/resume conformance gap-fill (S-11, #1847).** Adds seven
  paired `.harn` / `.expected` fixtures under
  `conformance/tests/agents/` covering the spec table from #1847 that
  was not already exercised by existing fixtures:
  `suspend_midloop_basic` (parent-driven `subagent_pause` /
  `subagent_resume` against a self-parked child),
  `suspend_nested` (two simultaneously-suspended workers resumed
  independently), `suspend_timeout_resume_with_summary` and
  `suspend_timeout_fail` (auto-resume timeout firing under
  `mock_time` + `advance_time`, exercising both `on_timeout` actions),
  `suspend_continue_transcript_false` (agent-loop level transcript
  reset on resume), `suspend_close_while_suspended` (close + resume
  rejects with HARN-SUS-010), `suspend_double_resume_race`
  (serialised double-resume rejects with HARN-SUS-003 — the pure-Harn
  observable surface of the HARN-SUS-006 race that Rust unit tests
  cover concurrently), and `suspend_daemon_step_parity` (post-S-07
  daemon snapshot field-set). All fixtures are deterministic — no
  wall-clock sleeps; the timeout path uses the unified mock clock
  through `clock::sleep`.
- **`InterruptAndSuspend` trigger handler variant (#1910).** Adds a new
  `TriggerHandlerSpec::InterruptAndSuspend` variant + the matching
  `InterruptAndSuspend({...})` constructor in `std/triggers`, completing the
  CH-10 leg of epic #1870 (Agent channels & periodic prompts). Unlike
  `Local`/`Worker`/`SpawnToPool` (which spawn new work) and
  `ReminderInject` (which delivers at the next turn boundary), this is the
  org-scoped "panic" broadcast: when the trigger matches, every running
  worker in the resolved `target_agents` scope is cooperatively suspended
  synchronously via the same `suspend_signal` + `WorkerSuspension` pipeline
  used by `suspend_agent` (#1837), bypassing the normal turn-boundary
  approval gate. `target_agents` accepts the string `"all"` (every worker
  in the local registry — the default), a list of concrete worker-id
  strings, or a closure `event -> list<string>` for dynamic resolution.
  `reason` is propagated to every suspended worker's `WorkerSuspension`
  envelope, the `WorkerSuspended` lifecycle event, and the per-suspension
  `triggers.interrupt_and_suspend.audit` audit entry. Already-suspended
  and terminal workers are skipped as `already_suspended` / `not_running`
  (no double-suspend, no overwritten reason), and stale ids returned by a
  closure are skipped as `unknown` rather than failing the broadcast.
  Empty target lists record a single roll-up audit and return a
  successful `status: "broadcast"` with `suspended_count: 0`. The
  per-worker iteration walks the registry in `BTreeMap` (sorted) order so
  the panic broadcast is deterministic across replay. Conformance:
  `conformance/tests/triggers/trigger_interrupt_and_suspend_{empty_scope_records_audit,all_scope_with_empty_registry,closure_scope}.harn`;
  Rust unit tests in `crates/harn-vm/src/stdlib/agents.rs` cover the
  per-worker `Suspended` / `AlreadySuspended` / `NotRunning` / `Unknown`
  outcomes and the deterministic registry-enumeration contract.
- **Pool tasks wired into `harness.unsettled_state()` (#2007).** Closes
  the PL-07 follow-up from #2008: `UnsettledStateSnapshot` now carries a
  `pool_pending_tasks` bucket fed by a new
  `crate::stdlib::pool::snapshot_pending_tasks()` helper that walks the
  thread-local pool registry and emits one entry per queued or running
  task (terminal tasks are skipped). The bucket exposes `pool_id`,
  `pool_name`, `task_id`, `status`, `priority`, optional `key` /
  `idempotency_key`, `submitted_at` / `submitted_at_ms`,
  `submitted_by`, optional `started_at`, and an `age_ms` derived from
  the wall clock at snapshot time. `harness.is_empty(state?)`,
  `harness.counts(state?)`, and `harness.summary(state?)` learn the
  new `pool_pending` count; `std/lifecycle`'s `is_empty(state)`,
  `counts(state)`, and `summary(state)` helpers learn the same field
  so script-land callbacks observe pool work alongside suspended
  sub-agents, queued triggers, partial handoffs, and in-flight LLM
  calls. Drops the `@xfail` marker on
  `conformance/tests/pool_unsettled_state_integration.harn` (now
  asserting `pool_pending=3` end-to-end) and decrements
  `scripts/xfail_threshold.txt` from 2 to 1.
- **Tool-hook LLM classifier (TH-05, #1898).** Adds the opt-in
  `llm_classifier` config to `ToolHooks.preset_run_command(...)` so
  ad-hoc commands that don't match any deterministic rule can be
  reviewed by a small model. The classifier is given the command +
  configurable `meta_prompt` and must return a one-line JSON verdict
  `{kind: "rewrite"|"deny"|"allow", confidence, rewritten?,
  explanation?}`. Confident `rewrite` / `deny` verdicts dispatch
  through the existing `tool_hooks_mode_*` callbacks (so audit log and
  reminder hooks fire exactly as for catalogue-matched rules);
  sub-threshold and `allow` verdicts audit + pass through to `inner`.
  Every invocation — including transport errors and cache hits — emits
  a `tool_hook_classifier_verdict` lifecycle audit entry
  (`{scope, model, command, normalized_command, kind, confidence,
  threshold, cache_hit, action, error?}`) so operators can observe
  classifier decisions in `pipeline_lifecycle_audit_log_take()` and
  event-log replay. Verdicts are cached in a thread-local map keyed by
  `<scope>:<sha256(normalized command)>` with optional
  `cache.ttl_ms` / `cache.ttl_seconds` so repeat commands skip the
  extra model call entirely. Transport errors (e.g. provider 5xx) are
  handled via `llm_call_safe` and degrade silently to passthrough
  rather than throwing. Trust contract: the raw command text + the
  meta-prompt are sent to the configured model, so callers must redact
  secrets in the same way they already do for `run_command`. Budget:
  each classifier call counts against the session's standard LLM cost
  telemetry (`peek_total_cost`, autonomy budget) — keep the classifier
  model small (the spec defaults to `claude-haiku-4-5-20251001`). The
  TH-02 rejection check is replaced by a config validator that throws
  on empty `model` or out-of-range `threshold` at wrapper-build time.
  New conformance fixtures: `tool_hooks_llm_classifier_rewrite`,
  `tool_hooks_llm_classifier_deny`,
  `tool_hooks_llm_classifier_passthrough` (covers below-threshold,
  explicit `allow`, and cache replay), and
  `tool_hooks_llm_classifier_error_recovery`. Part of epic #1884
  (Preset tool hooks library).
- **Channel replay determinism receipts + diagnostics (#1878).** CH-07 of
  the channels epic (#1870) closes the audit/replay gap by giving every
  `emit_channel(...)` and every channel-source trigger match a durable
  receipt on the new `lifecycle.channel.audit` event-log topic. Adds
  two receipt types: `ChannelEmitReceipt` carries the resolved name,
  scope, scope id, the full emit payload, a canonical-JSON SHA-256
  `payload_hash`, the signed `emitted_at` timestamp (reusing the
  per-session HMAC salt from CH-01 / #1872), the active emit span id,
  and the pipeline/session/tenant correlation fields; and
  `ChannelMatchReceipt` carries the same `event_id` (so the cached
  match keys off the emit's id rather than re-evaluating the filter
  spec on replay), the recorded `trigger_id` + `binding_key` +
  `handler_kind`, a `handler_result` summary recorded after the
  dispatcher returns (`status` / `attempt_count` / `error` /
  `dispatch_failed`), the signed `matched_at` timestamp bound to the
  `(event_id, trigger_id)` pair, and a `batch.constituent_event_ids`
  list for aggregation/batched triggers (CH-04 / #1875) so replay
  reconstructs the batch from those ids. Each audit append rides the
  active event log (`active_event_log()`) so receipts inherit the
  pipeline's durability — in-memory in tests, durable in production.
  The match dispatch path also resolves `event_id` from the trigger
  event's `dedupe_key` (which carries the channel's emit id) rather
  than the freshly-minted `trigger_evt_*` UUID, so the receipt chain
  links emit -> match by stable id. In parallel, extends the
  replay-oracle (`crates/harn-vm/src/orchestration/replay_oracle.rs`)
  with a first-class `channel_receipts` section on `ReplayTraceRun`
  (and matching `ReplayTraceRunCounts` + `replay_bench.rs` runtime-cost
  metrics + section list) so multi-agent channel exchanges round-trip
  byte-identically across two runs of the same workload; and adds the
  typed `ChannelReplayDiagnostic` enum surfaced via
  `diagnose_channel_replay_drift(...)` with three codes:
  `HARN-REP-CHN-001` (replay matched an `event_id` with no recorded
  emit), `HARN-REP-CHN-002` (replay emit `payload_hash` drifted from
  the recorded hash — producer code drift), and `HARN-REP-CHN-003`
  (batched-match `constituent_event_ids` composition drifted between
  runs). Three new conformance fixtures under
  `conformance/tests/triggers/trigger_channel_replay_*` cover the
  emit-receipt shape + signed timestamp, the emit -> match
  `event_id` linkage with `handler_result.status="succeeded"`, and the
  payload-hash stability/drift contract (identical payloads produce
  identical hashes; flipped fields produce different hashes; dict key
  ordering is canonicalized so iteration order can't poison the
  oracle). Five new unit tests under `replay_oracle::tests` cover the
  diagnostic codes end-to-end. Unblocks CH-08 conformance and the
  end-to-end multi-agent replay / audit / compliance use cases the
  epic was driving toward.
- **Lifecycle replay determinism (P-08, #1861).** Closes the
  pipeline-lifecycle epic's #1853 replay-determinism leg with the
  journal entry types every SOTA replay engine (Temporal, Restate,
  Inngest, Azure Durable, Cadence) ships: `SuspensionReceipt`,
  `ResumptionReceipt`, and `DrainDecisionReceipt`, each carrying a
  `SignedLifecycleTimestamp` whose HMAC binds
  `(kind, at_ms, subject_id, initiator_id)` under a per-process salt.
  `ResumptionReceipt` records both the cached resume input and its
  canonical-JSON `input_hash` so replay can re-feed the same payload
  into the suspended worker instead of re-prompting; map-key order
  drift is absorbed by the canonical hash. `DrainDecisionReceipt`
  captures the settlement agent's chosen `action` and an optional
  `prompt_hash` so replay can short-circuit the LLM call. New Rust
  builtins `lifecycle_receipt_record_{suspension,resumption,drain_decision}`,
  `lifecycle_receipts_snapshot`, `verify_lifecycle_receipt_signature`,
  `lifecycle_resume_input_hash`, `lifecycle_drain_decision_prompt_hash`,
  `lifecycle_replay_resume_input`, and `lifecycle_replay_drain_decision`
  wire the receipt model into Harn pipelines. Drift surfaces three new
  diagnostic codes — `HARN-SUS-011` (resume input hash mismatch),
  `HARN-SUS-012` (drain decision prompt hash mismatch), and
  `HARN-SUS-013` (lifecycle signature verification failure) — and the
  replay oracle treats `lifecycle_receipts` as first-class trace
  material alongside event log entries and llm interactions. Three
  conformance fixtures (`lifecycle_replay_record_and_replay`,
  `lifecycle_replay_cached_resume_input`,
  `lifecycle_replay_signed_timestamps`) pin the determinism contract:
  byte-identical receipt snapshot across record/replay, cached resume
  input round-trip with canonical-hash drift detection, and signed
  timestamps that stay pinned to the original wall clock even after
  the live clock has advanced. Privacy: `ResumptionReceipt` supports
  an optional `RedactionPolicy` that scrubs sensitive paths from the
  persisted `input` while leaving the hash computed against the
  original — replay still works, secrets do not land in the journal.
- **OTel suspend-end / resume-link wiring (S-18, #1867).** Wires the
  `SpanKind::Suspension` and `SpanKind::Resume` spans (added in P-05,
  #1858) into the cooperative `suspend_agent` / `resume_agent` paths.
  The suspension span is now closed before the snapshot is persisted
  (Temporal community best-practice — never carry one root span across
  a multi-day pause) and the resume path opens a fresh detached
  `SpanKind::Resume` span that links — not parents — back to the prior
  suspension span and the pipeline span that was active at suspend
  time. Adds the canonical attribute bag to both spans: suspend carries
  `handle`, `reason`, `initiator`, `has_conditions`, `pipeline_id`
  (alias of `pipeline_span_id`), and `parent_worker_id`; resume carries
  `handle`, `initiator`, `continue_transcript`, `had_resume_input`, and
  `linked_suspension_count`. Privacy: the `resume_input` value itself
  is deliberately never serialised onto resume-span attributes — only a
  boolean flag — and the existing `WorkerSuspension` JSON round-trip
  already preserved `prior_span_link` + `pipeline_span_link` from
  P-05, so cross-process cold resume continues to link to the closed
  pre-restart span. New `suspend_otel_links` conformance fixture
  exercises the warm suspend → resume cycle from script land. Part of
  epic #1836 (Agent suspend/resume).
- **Pipeline lifecycle conformance gap-fill (#1862).** P-09 closes the
  pipeline-lifecycle epic (#1853) by landing the spec-named umbrella
  fixtures on top of the per-preset coverage that already shipped with
  P-01..P-07: `pipeline_lifecycle_default` (default `on_finish: drain`,
  no unsettled → `pipeline_finalized` audit + PreFinish/PostFinish fire
  around the callback), `pipeline_lifecycle_abandon` (`on_finish_abandon`
  surfaces a `pipeline_abandoned_unsettled` audit with the typed counts),
  `pipeline_drain_decision_callback` (settlement-agent `emit_audit(
  "drain_decision", ...)` records the documented shape AND the
  `OnDrainDecision` hook dispatches with `action`/`item` payload),
  `pipeline_composed_finish_handlers` (`compose([log, drain, audit])`
  threads through `pipeline_on_finish` in the declared order), and
  `pipeline_replay_equivalence` (lifecycle audits persisted to the
  `pipeline.lifecycle.audit` event-log topic round-trip byte-identical
  through `event_log.subscribe(from_cursor, kind_prefix)` with monotonic
  seq). `pipeline_drain_ordering_enforcement` lands as `@xfail`
  referencing #1856 (P-03 settlement-agent loop) — the fixture pins the
  post-wiring HARN-DRN-001 contract so dropping the marker after #1856
  lands is the only edit needed. Together with the existing
  `on_finish_preset_*`, `combinator_*`, `lifecycle_hook_events_*`,
  `lifecycle_event_shape_*`, and `on_budget_*` fixtures the lifecycle
  surface is now executable spec end-to-end.
- **OAuth dynamic client registration + auto-hosted metadata (#1906).**
  Closes OA-05 on epic #1885 with the worker-side dual of `std/oauth/client`:
  a new `std/oauth/dynamic_registration` module that builds RFC 7591
  client metadata, RFC 8414 authorization-server metadata, and runs an
  in-process RFC 7591 dynamic registration store. `client_metadata(opts)`
  validates + defaults a candidate document for serving at
  `/.well-known/oauth-client.json`; `authorization_server_metadata(provider,
  overrides?)` derives the server document from a `std/oauth/providers`
  record for `/.well-known/oauth-authorization-server.json`;
  `dynamic_registration_store()` + `register_client(store, metadata)` issue
  fresh `client_id` / `client_secret` pairs (256-bit URL-safe base64) with
  RFC 7591 §5.1 server-side latitude to reject non-HTTPS redirect URIs
  (except RFC 8252 §7.3 loopback), fragments, and non-spec
  `grant_types` / `response_types` / `token_endpoint_auth_method` values.
  All rejections thread through a stable `HARN-OAU-005:` diagnostic
  prefix. The `client_secret` is only returned by `register_client`;
  subsequent `get_client` reads omit it, and the `oauth.dynreg.audit`
  `client_registered` event carries only the redacted shape
  (`redirect_uri_count`, `grant_types`, `has_client_secret`) — never the
  credential material. `well_known_paths()` + `well_known_response()`
  return the canonical URL paths and an HTTP envelope that embedders
  (harn-cloud, future `harn serve --oauth-resource ...`) can mount
  directly. New Rust builtins (`__oauth_dynreg_store_handle`,
  `__oauth_dynreg_register`, `__oauth_dynreg_get`, `__oauth_dynreg_list`,
  `__oauth_dynreg_validate_metadata`, `__oauth_dynreg_build_client_metadata`,
  `__oauth_dynreg_build_authorization_server_metadata`) are registered
  in the parser signature table and live in
  `crates/harn-vm/src/stdlib/oauth_dynreg.rs`; `client_id_issued_at`
  routes through `stdlib::clock` for mock-clock-deterministic
  conformance. Three new fixtures under `conformance/tests/stdlib/`
  (`oauth_dynamic_registration_metadata`, `..._register`, `..._rejects`)
  cover well-known builder defaulting, happy-path registration with
  audit-shape redaction, and the RFC 7591 §5.1 / §2 rejection matrix.
  Intentional cut: the harn-serve port-wiring (a `/well-known` route
  group + `POST /register` handler) is deferred to a follow-up so this
  PR can ship the stdlib + validation surface that both harn-cloud and
  harn-serve will share without coupling the two. Embedders consume the
  artifacts produced here as opaque `{status, content_type, headers,
  body}` envelopes.
- **Channel emit/match transcript events + OTel spans (#1877).** CH-06 of
  the channels epic (#1870) closes the debuggability gap for channels by
  giving every emit and match first-class observability on the same
  primitives the rest of the runtime uses. Adds two new `SpanKind`
  variants — `ChannelEmit` (opened at `emit_channel(...)`, closed once
  the durable append + trigger fan-out finishes) and `ChannelMatch`
  (opened just before the channel-source trigger dispatcher invokes the
  handler, closed when dispatch returns) — surfaced through
  `trace_spans()` and the OTel exporter. `ChannelMatch` spans link back
  to the originating `ChannelEmit` span via `set_span_link` (P-05 /
  #1858) using the Harn `harn.channel.emit_trace_id` /
  `harn.channel.emit_span_id` headers stashed on the trigger event; for
  aggregation/batched triggers (CH-04 / #1875) the match span
  multi-links to every constituent emit span so the trace tree shows
  the full fan-in. In parallel, emits two new transcript event kinds —
  `transcript.channel.emit` (every append, whether fresh or
  idempotent-duplicate, with `payload_summary`, `inserted`, `duplicate`,
  `emitted_at`, `emitted_by`, scope/session/pipeline/tenant correlation,
  and the live `span_id`) and `transcript.channel.match` (every handler
  dispatch, with `trigger_id`, `handler_kind`, `matched_at_ms`,
  `matched_in_session_id`, the live `span_id`, and a `batch` summary
  carrying `count` + `constituent_event_ids` for aggregated dispatch) —
  on the new `transcript.channel.lifecycle` event-log topic, mirroring
  the `transcript.reminder.lifecycle` pattern from S-15 (#1865).
  Window-expire flushes of aggregation buffers (`fire_partial` /
  `discard` paths) reconstruct the `ResolvedChannel` from the first
  buffered event so the match span carries the right scope + name even
  when the flush dispatches off a background sweep. Three new
  conformance fixtures under
  `conformance/tests/triggers/trigger_channel_otel_*` cover the
  single-event emit/match link, the batched multi-link multi-emit
  case, and the idempotent re-emit (transcript event still fires with
  `inserted=false`, but fan-out is suppressed so no follow-on match
  event lands). Unblocks CH-07 audit, CH-08 conformance, and B-08 IDE
  rendering.
- **Pool conformance gap-fill: `pool_unsettled_state_integration` (#1892).**
  PL-07 (epic #1883) is the comprehensive pool conformance suite. Eight of
  the ten spec scenarios were already covered by fixtures landed alongside
  PL-01..PL-06: concurrency cap (`pool/max_concurrent_caps_in_flight`),
  priority + LIFO + fair round-robin (`pool/priority_submit`,
  `pool/queue_strategy_submit`), the full backpressure matrix including
  block/drop-oldest/drop-newest/fail-fast/fail-submitter/ring-buffer plus
  drop-receipt audit (`pool/backpressure_policies`), durable pipeline-scope
  restart (`pool_durability/pipeline_survives_restart`), and trigger →
  pool fan-in (`triggers/trigger_spawn_to_pool_*`). The new
  `conformance/tests/pool_unsettled_state_integration.harn` covers the last
  scenario — pool tasks visible in `harness.unsettled_state()` so on_finish
  presets can act on them — and is marked `@xfail` referencing follow-up
  #2007 until the runtime wiring lands.
- **OAuth conformance suite + explicit revoke + `HARN-OAU-002` (#1908).**
  Closes the OA-07 gap-fill on top of the OA-01..OA-06 surface: adds
  three new conformance fixtures — `oauth_refresh_token_expired`
  (server returns `invalid_grant` on the refresh-grant call, raises
  the dedicated `HARN-OAU-002` diagnostic and leaves the stored
  TokenSet untouched), `oauth_revoke` (RFC 7009 best-effort POST to
  the provider's `revoke_url`, authoritative local storage delete,
  `oauth.client.audit` `token_revoked` event with redacted prev shape,
  and a forced `HARN-OAU-002` on the next `token(cli)` call), and
  `oauth_pkce_validates` (mocks the server's mismatched-verifier
  rejection and asserts the error_description threads through with
  no audit and no persisted TokenSet). Promotes the previously
  private `cli.revoke()` storage-delete shim into a real
  `pub fn revoke(cli)` that issues the RFC 7009 token-revocation POST
  on a best-effort basis (network failures never block the local
  discard) and emits an audit event. Tags both the
  no-refresh-token-in-storage and the failed-refresh-grant paths in
  `std/oauth/client` with the new `HARN-OAU-002` diagnostic prefix so
  scripts can pattern-match on a single, stable string instead of
  parsing freeform error text. The fifteen `oauth_*` conformance
  fixtures together cover authorization-code + PKCE, device flow
  (happy/pending/slow-down/expired), refresh on 401, pre-emptive
  refresh at 75 % TTL, refresh-token expiry, revoke, PKCE mismatch,
  storage round-trip across all four backends (memory/file/cloud
  session/cloud org/custom), the provider catalogue against ten
  preconfigured providers, and the OA-06 redaction patterns. Part of
  epic #1885 (OAuth stdlib).
- **`ReminderInject` trigger handler variant (#1876).** Adds a new
  `TriggerHandlerSpec::ReminderInject` variant + the matching
  `ReminderInject({...})` constructor in `std/triggers`, completing the
  CH-05 leg of epic #1870 (Agent channels & periodic prompts). Unlike
  Local/Worker/SpawnToPool handlers, ReminderInject does not spawn a
  task: when the trigger matches, the dispatcher resolves a target
  session (`"current"`, `"parent"`, a literal session id, or a closure
  `event -> string?`), renders a `.harn.prompt` body template against
  `{{ event }}` / `{{ match }}` / `{{ batch }}`, builds a
  `SystemReminder` (#1815) carrying the rendered body plus the
  binding's `tags` / `ttl_turns` / `dedupe_key` / `propagate` /
  `role_hint` / `preserve_on_compact` metadata, and injects it via
  `agent_sessions::inject_reminder`. The reminder surfaces at the
  target session's next turn boundary — same path as
  `transcript.inject_reminder` — so existing dedupe, TTL, and
  capability-aware rendering all apply. Missing target sessions are
  recorded as `triggers.reminder_inject.audit` lifecycle audit entries
  (outcome `dropped`, reason `target_missing` or
  `target_unknown_session`) and the dispatch returns a `dropped`
  result rather than failing the trigger. Combined with CH-04 batching
  this enables declarative "every N events OR every T time" reminder
  injection without user-side counter state. Conformance:
  `conformance/tests/triggers/trigger_reminder_inject_{targets_concrete_session,only_targets_named_session,missing_target_drops_with_audit,closure_target}.harn`.
- **Aggregation triggers — `batch { count, window, key, expire_action }` filter (#1875).**
  Implements CH-04 from epic #1870. New optional `batch` field on the
  trigger DSL accumulates matching channel events into a per-(binding,
  partition_key) buffer; the handler fires with a batched
  [`TriggerEvent`](https://github.com/burin-labs/harn) (`event.batch`
  populated) once `count` is reached or the `window` elapses. `key` is a
  dot-path into the channel payload (e.g. `"repo"`,
  `"pull_request.user.login"`); missing path = single global counter for
  the binding. `expire_action` defaults to `"fire_partial"` (handler
  invoked with the partial batch) and can be set to `"discard"` to drop
  the buffer silently. Window expiration is driven by an implicit sweep
  during the next `emit_channel(...)` and by the explicit
  `flush_trigger_aggregations()` builtin (deterministic + paired with
  `mock_time` / `advance_time` for replay-clean tests). Buffers are
  capped at 1024 events per partition and overflow is reported as a
  structured `triggers.aggregation.buffer_overflow` warning. Bad config
  (count ≤ 0, missing/unparseable `window`, unknown `expire_action`,
  wrong types) raises `HARN-CHN-005` at registration. Inngest-shape
  primitive — no other major durable-execution or agent system has
  first-class fire-after-N-events. Conformance:
  `conformance/tests/triggers/trigger_batch_{count_fires_on_threshold,window_expire_fires_partial,window_expire_discards,key_partitions_independently,back_to_back_resets_buffer,malformed_count_errors}.harn`.
- **Channel scope resolver completes the four-tier hierarchy (#1874).**
  Formalises the `session < pipeline < tenant < org` scope chain for
  `emit_channel(...)` and `channel_events(...)`, building on the producer
  (#1871) and trigger-source consumer (#1872) halves. The resolver is now
  fully deterministic: bare names default to tenant scope; explicit prefixes
  are honoured; `pipeline:` outside a pipeline context fails with
  `HARN-CHN-001`; cross-tenant emits without a grant and any `org:` use fail
  with `HARN-CHN-002`; malformed scope strings fail with `HARN-CHN-003`; and
  a new `HARN-CHN-004` fires when `options.session_id` or
  `options.pipeline_id` explicitly disagrees with the active runtime
  context (previously, an explicit option would silently override the
  context — a quiet path to cross-session leakage). Cross-scope isolation is
  enforced by the topic shape (`channels.<scope>.<scope-id>.<name>`), so
  readers against a different `tenant_id`, `session_id`, or `pipeline_id`
  see an empty view rather than a leaked event. Conformance coverage:
  `conformance/tests/stdlib/channel_scope_{bare_defaults_to_tenant,pipeline_outside_pipeline_errors,cross_tenant_isolation,cross_session_isolation}.harn`
  plus the existing `emit_channel_scope_resolution.harn`. Part of epic
  #1870.
- **Callback-first `ResumeBy.*` exports (#1864).** The new
  `std/agent/resume_by` module replaces implicit resume-responsibility
  inference with four explicit `(harness, suspension) -> dict`
  callbacks: `ResumeBy().parent_llm` (surface the suspension to the
  parent transcript and let the parent's LLM resume),
  `ResumeBy().local_runtime` (register conditions with the in-process
  #152 dispatcher; declines with `no_conditions` when none are set),
  `ResumeBy().cloud_harness` (register with the harn-cloud webhook
  receiver; declines with `no_cloud_session` until HC-07..HC-09 wire
  the back end), and `ResumeBy().pipeline_drain` (defer to the
  enclosing pipeline's drain step). Each callback emits a
  `resume_by_dispatched` or `resume_by_declined` audit through
  `harness.emit_audit` for observability, and the
  `agent_await_resumption(reason, conditions, resume_by)` request now
  carries the callback for the agent loop to invoke at suspend time
  with `invoke_resume_by(...)` (with a `parent_llm` safety-net
  fallback). `first_handled([...])` and `default_resume_by({...})`
  helpers ship in the same module so chains like
  `first_handled([ResumeBy().cloud_harness, ResumeBy().local_runtime])`
  compose with the existing `std/lifecycle/combinators`. Conformance:
  `conformance/tests/agents/resume_by_{parent_llm,local_runtime,cloud_harness,pipeline_drain,composed,default}.harn`.
  Part of epic #1836 (agent suspend/resume) and #1853 (callback-first).
- **`OnBudget.*` callback strategies (#1914).** Adds three named callback
  strategies for the existing `OnBudgetThreshold` lifecycle event in a
  new `std/lifecycle/on_budget` module: `OnBudget.terminate` (emits
  `budget_exceeded` audit, throws a structured `budget_exceeded`
  exception so the surrounding agent loop / pipeline unwinds),
  `OnBudget.graceful_exit` (emits `budget_graceful_exit` audit, returns
  a deterministic `{status: "budget_exhausted", strategy:
  "graceful_exit", reason, budget_state, message}` envelope so the
  `on_finish` chain can drain in-flight work and surface the envelope
  as the pipeline's value), and `OnBudget.warn_and_continue` (emits
  `budget_warn_and_continue` audit, injects a 1-turn `budget_warning`
  system_reminder via `tool_hooks_inject_reminder`, and returns the
  original `budget_state` unchanged so combinator chains see a
  passthrough). All three follow the same `(harness, budget_state) ->
  result` shape that the rest of the lifecycle layer uses, so they
  compose freely with `std/lifecycle/combinators` —
  e.g. `compose([OnBudget.warn_and_continue, custom_logger])` threads
  the dispatcher's snapshot through both entries. A
  `OnBudget()` namespace factory mirrors the `QueueStrategy()` /
  `Backpressure()` factories in `std/lifecycle/pool` for dotted-access
  callers. Conformance coverage at
  `conformance/tests/on_budget_{terminate,graceful_exit,warn_and_continue,compose}.harn`.
  Part of epic #1853 (Pipeline lifecycle).
- **`std/oauth/device_flow` RFC 8628 device authorization grant (#1903).**
  Adds `OAuth.device_flow(provider, opts)` for headless contexts (CI
  runners, daemons, IDE side panes) where a browser redirect is
  impractical. Posts to the provider's device authorization endpoint,
  hands `(user_code, verification_uri)` to a caller-supplied
  `on_user_code` handler (defaults to `eprintln(...)` with the URL and
  code), polls the token endpoint at the server-supplied `interval`,
  honors `slow_down` by adding 5 s to the cadence, treats
  `authorization_pending` as a soft retry, and raises a deterministic
  error on `expired_token` / `access_denied`. On success, builds a
  TokenSet (mirroring the `std/oauth/client` exchange path) and persists
  it through the OA-03 storage handle so `OAuth.client(...)` reads the
  same token + refresh metadata back. Emits an
  `oauth.device_flow.audit` `token_obtained` event log entry whose
  payload includes provider, storage key, and token presence flags but
  never the access token, refresh token, device code, or user code. The
  polling sleep routes through the standard `sleep(ms)` builtin so
  tests under `mock_time(...)` / `advance_time(...)` exercise the
  cadence without real wall-clock waits. Conformance:
  `conformance/tests/stdlib/oauth_device_flow_{happy_path,pending,slow_down,expired}.harn`.
  Part of epic #1885 (OAuth stdlib).
- **Pool OTel spans + audit receipts (#1891).** Adds two new
  `SpanKind` variants — `PoolSubmit` (fires at `pool.submit()`, carries
  `pool` / `pool_id` / `priority` / `key` / `idempotency_key` /
  `task_id` / `status` metadata) and `PoolDequeue` (fires when the
  dispatcher pulls a task out of the queue, carries the same
  `pool` / `pool_id` / `task_id` plus `queued_for_ms` and `slot_index`,
  and links back to the originating `PoolSubmit` span via
  `set_span_link` from P-05 / #1858 so submit → dequeue stays
  correlated across the async boundary). Three audit receipts now land
  on the existing `lifecycle.pool.audit` topic: `pool_submit`
  (`harn.pool_submit.v1`), `pool_dequeue` (`harn.pool_dequeue.v1`),
  and `pool_drop` (`harn.pool_drop.v1`, already shipped in PL-03).
  All three respect the disabled-tracing fast path (the span guards
  return id 0 when tracing is off). The `event_log.subscribe`
  `kind_prefix` filter lets observers narrow the topic to a specific
  receipt kind (the existing backpressure-audit fixtures use
  `kind_prefix: "pool_drop"` so they ignore the new receipts).
  Conformance coverage: `conformance/tests/pool_otel/*.harn`. Part of
  epic #1883.
- **Pool state durability (#1890).** `Pool.create({scope: ...})` now
  routes to three durability backends following the channel scope
  contract: `session` (default, in-memory only — lost on session close);
  `pipeline`, which appends task metadata to a crash-safe JSONL log
  under `.harn/pools/<pipeline_id>__<name>__<hash>.jsonl` and reloads
  the queue + terminal state on next `pool_create`; and `tenant` / `org`,
  reserved for the harn-cloud host (see harn-cloud#306) and rejected
  today with a clear host-routed diagnostic. Submissions accept a new
  `idempotency_key` option that dedupes both within a session and
  across pipeline-scope reloads. In-flight tasks observed on reload are
  classified as `failed` with a stale-restart marker (closures cannot
  cross a process boundary); idempotent re-submission lets the new
  process re-run the work. A `pool_simulate_restart()` builtin drops
  the in-process registry so conformance fixtures can exercise the
  "kill process → restart → verify completion" path without forking.
  Crash safety uses atomic temp-file rename for compaction and
  `sync_data()` per append. See
  `conformance/tests/pool_durability/*.harn`. Part of epic #1883.
- **Tool-hook seed catalogues for the preset library (#1897).** Ships
  the initial "command faux-pas" rule sets that back
  `preset_run_command(stacks: [...])`. Seven catalogues land under
  `crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn`:
  `harn-canon/rust` (cargo target-dir, test capture, clippy workspace,
  fmt --check), `python` (find vs rg, pip install --user, pytest
  capture), `typescript` (find vs rg, npm install --force /
  --legacy-peer-deps), `swift` (swift build/package clean), `sql`
  (unbounded `SELECT *`), `harn` (cargo run --bin harn missing
  --quiet), and a stackless `universal` catalogue carrying two
  severity=error deny rules — `git push --force` against `main` /
  `master` (with a `HARN_TOOL_HOOKS_ALLOW_FORCE_PUSH=1` escape hatch
  and `--force-with-lease` allowlist) and recursive `rm -rf` against
  root-adjacent paths (`/`, `~`, `..`, `$HOME`, `${HOME}`, `*`,
  resistant to quoting bypasses). A new
  `tool_hooks_seed_registry(stacks)` helper composes the requested
  catalogues plus universal; `preset_run_command(...)` auto-seeds from
  it when no explicit `registry:` override is passed, so opting into a
  stack is a one-liner. Each rule carries an `explanation` the agent
  can paraphrase and a `references` list pointing at upstream docs /
  RFCs / post-mortems. Conformance coverage:
  `conformance/tests/stdlib/tool_hooks_catalogue_{rust,python,typescript,swift,sql,harn,universal,auto_seed}.harn`.
  Part of epic #1884 (preset tool hooks library).
- **Channel-source triggers (#1872).** The trigger DSL now recognizes a
  `provider: "channel"` source with a `match.events: ["channel:<scope>:<name>"]`
  selector, completing the consumer half of the durable channel API shipped
  in #1871. Supported selector shapes: `channel:<name>` (tenant-default),
  `channel:session:<name>`, `channel:pipeline:<name>`,
  `channel:tenant:<tenant-id>:<name>`, `channel:tenant:*:<name>` (tenant
  wildcard), and `channel:org:<org-id>:<name>` (rejected until org grants
  ship, mirroring the producer-side `HARN-CHN-002`). Each `emit_channel(...)`
  that successfully appends to the event log synchronously fans out to all
  matching active bindings; idempotent duplicates short-circuit the
  fan-out. An optional dict-shaped `filter:` (e.g. `"{\"repo\": \"harn\"}"`)
  evaluates dot-path equality against the emit payload before dispatch.
  Conformance coverage:
  `conformance/tests/triggers/trigger_source_channel_{basic,session,wildcard,filter,fan_out}.harn`.
  Part of epic #1870.
- **OAuth token redaction in transcripts and audit (#1907).** Persisted
  transcripts, audit receipts, OTel span attributes, and system reminders
  now route through a named token-pattern catalog so a leaked OAuth bearer,
  GitHub PAT (classic or fine-grained), Slack `xox*`, AWS `AKIA…`, Stripe
  key, OpenAI `sk-…`, JWT, GitLab `glpat-…`, or npm `npm_…` token is
  scrubbed to `<redacted:<pattern>:<len>>` on every display sink. The
  underlying tool/host call still receives the raw token — redaction is
  display-only. Each redaction synchronously records an `HARN-OAU-001`
  audit entry in a per-thread ring drainable via the new
  `std/oauth/redaction` stdlib (`register_pattern`, `redact`,
  `default_patterns`, `custom_patterns`, `clear_custom_patterns`,
  `drain_audit`). When a multi-threaded Tokio runtime is available the
  default sink also appends to the `audit.token_redaction` event-log
  topic. Inputs over 256 KiB short-circuit to a passthrough as defense
  against catastrophic regex behavior. Conformance:
  `conformance/tests/stdlib/token_redaction_{default_patterns,custom_pattern,tool_passthrough}.harn`.
  Part of epic #1885 (OAuth stdlib).
- **Tool-hook mode callback side effects (#1896).** The three shipped
  `tool_hooks_mode_*` callbacks (`rewrite_with_audit`,
  `deny_with_explanation`, `passthrough_only_audit`) now emit lifecycle
  audit entries (`tool_rewrite`, `tool_denied`, `tool_rule_warning`)
  observable via `lifecycle_audit_log_take()`. The rewrite mode also
  queues a 1-turn `tool_rewritten` system reminder via the new
  `tool_hooks_inject_reminder(...)` primitive so the agent's next turn
  learns the corrected command shape. Without an active agent session
  the reminder still records a `tool_hooks.reminder_injected` audit
  entry, so headless pipelines and conformance fixtures can verify
  every side effect deterministically. Two new top-level builtins,
  `tool_hooks_emit_audit(kind, payload)` and
  `tool_hooks_inject_reminder(options)`, are exposed so user-defined
  mode callbacks can hook into the same plumbing. See
  `conformance/tests/stdlib/tool_hooks_mode_callbacks.harn` for the
  executable spec. Part of epic #1884 (preset tool hooks library).
- **Protocol contribution RFCs for reminders (#1829).** Authored three
  upstream-proposal documents under `docs/src/protocol-contributions/`:
  ACP `session/inject_reminder` + `SessionUpdate::ReminderEmitted` (the
  schema-edit follow-up to [ACP #1224][acp-1224]), A2A
  `tasks/inject_reminder`, and MCP `notifications/reminder`. Each RFC
  describes the proposed wire format, migration path from our existing
  `_meta.harn.reminder` envelope, reference-impl status, and open
  questions for upstream maintainers. Upstream filings remain a
  maintainer action — see #1829 for the outstanding work.

[acp-1224]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224

- **Lifecycle hook events for suspend / resume / drain phases (#1859).**
  Adds seven new `HookEvent` variants — `PreSuspend`, `PostSuspend`,
  `PreResume`, `PostResume`, `PreDrain`, `PostDrain`, `OnDrainDecision`
  — wired through `register_session_hook` (snake_case names) and the
  shared dispatcher. The control surface gains `HookControl::Modify`
  so lifecycle gates can rewrite the dispatched payload before
  downstream consumers see it (rewrite suspend reason, amend resume
  input, amend drain spec, rewrite drain tool call, amend unsettled
  snapshot). `Block` cancels the gated operation where applicable
  (`PreSuspend`, `PreResume`, `PreDrain`, `OnDrainDecision`,
  `OnUnsettledDetected`); `PreFinish` explicitly rejects `Block` and
  surfaces a runtime error pointing the user at
  `OnFinish.block_until_settled`. Reminder effects continue to be
  inject-only. See `docs/llm/harn-quickref.md` for the per-event
  return-semantics table and `conformance/tests/hooks_session/
  lifecycle_hook_events_*` for executable spec coverage. Settlement
  agent spawning itself remains the P-03 deferred stub (harn#1856).
- **`std/lifecycle/combinators` callback combinator stdlib (#1860).**
  Six pure factories for wrapping `(harness, return_value)`-shaped
  callbacks (hook handlers, `resume_by`, `on_finish`, anywhere a
  function-shaped value reaches user code). `compose(callbacks)` runs
  callbacks sequentially and threads the return value through the
  chain; `first_available(callbacks)` returns the first non-nil
  responder; `with_telemetry(cb, span_name?)` opens a
  `SpanKind::FnCall` OTel span (via new `__lifecycle_span_start` /
  `__lifecycle_span_end` bridge builtins) and emits paired
  `{span_name}_started` / `_completed` / `_errored` audits;
  `with_timeout(cb, ms)` is a soft, clock-aware deadline that
  measures via `harness.clock.now_ms()` and returns a
  `{__timed_out, timeout_ms, elapsed_ms, return_value}` sentinel on
  overrun; `if_unsettled(cb)` only fires when `harness.unsettled_state()`
  has pending work (one snapshot per call); `when(predicate, cb)`
  guards on an arbitrary predicate. Conformance coverage at
  `conformance/tests/combinator_*.harn`. Part of the pipeline
  lifecycle epic (#1853, P-07).
- **`SpawnToPool` trigger handler variant (#1889).** Triggers now compose
  with named agent pools (#1883) via a new `TriggerHandlerSpec::SpawnToPool`
  variant, surfaced through the Harn DSL as
  `handler: SpawnToPool({pool: "pr-review", priority_from: "headers.priority",
  key_from: "tenant_id", task_factory: { event -> { -> review(event) } }})`.
  The dispatcher invokes `task_factory(event)` per matched event, extracts
  priority + fair-queue key via simple dotted JSON paths into the event
  payload (missing paths fall back to defaults rather than erroring), and
  submits the resulting closure to the pool under its configured queue
  strategy + backpressure policy. `drop_newest` rejections reuse the existing
  `lifecycle.pool.audit` channel, and the dispatch result is shaped as a
  `pool_task` handle so handlers can call `pool_wait(dispatch.result)`
  directly. Conformance:
  `conformance/tests/triggers/trigger_spawn_to_pool_*`.
- **`std/oauth/client` PKCE-S256 client + transparent refresh (#1902).**
  Adds `OAuth.client(provider, opts)` and `OAuth.token(client)` per
  RFC 6749 (authorization-code) + RFC 7636 (PKCE S256, always
  enforced) + RFC 9700 BCP refresh guidance. The client builds on the
  OA-04 `std/oauth/providers` catalogue and the OA-03
  `std/oauth/storage` backends so token persistence and rotation use
  the same primitives as the rest of the stack. `start_authorization`
  builds the PKCE-protected `/authorize` URL plus a one-shot
  `code_verifier / state` pair; `exchange_code` validates state, sends
  the code + verifier, and writes the resulting TokenSet to storage.
  `token(cli)` always reads storage as the source of truth so two
  in-process callers see the latest refresh, and pre-refreshes when
  the stored TokenSet is past 75% of its TTL. `request(cli, method,
  url, opts?)` injects a Bearer token and retries exactly once after a
  401, forcing a refresh between attempts. Every successful refresh
  or exchange emits a `token_refreshed` / `token_exchanged` event on
  the `oauth.client.audit` topic; audit payloads carry only presence
  flags + timestamps and never include the new access/refresh tokens.
  New supporting builtins in `std/crypto`: `bytes_to_base64url`,
  `sha256_base64url`, and `crypto_random_bytes(n)` (CSPRNG, capped at
  1024 bytes). Conformance coverage:
  `conformance/tests/stdlib/oauth_client_auth_code.harn`,
  `oauth_client_refresh_on_401.harn`,
  `oauth_client_refresh_near_ttl.harn`.
- **`std/oauth/storage` token storage backends (#1904).** Five
  interchangeable backends for the OAuth client (#1885 OA-03):
  `memory()` (per-VM, ephemeral), `file(path, encryption_key)` (single
  AES-256-GCM envelope with HKDF-SHA256 key derivation, atomic
  temp-file write + rename), `harn_cloud_session()` /
  `harn_cloud_org()` (route through the new `oauth_storage`
  `cloud_get / cloud_set / cloud_delete` host capability so the
  embedder enforces RLS), and `custom({get, set, delete, id?})` for
  vault / KMS integrations. Each backend exposes the same
  `store.get / store.set / store.delete` closures on the handle dict,
  letting the upcoming OAuth client treat storage as a single
  protocol. File-handle encryption keys live in a thread-local map
  off the user-visible handle so a stray `to_string(handle)` cannot
  exfiltrate the secret. See
  [`docs/src/stdlib/oauth-storage.md`](docs/src/stdlib/oauth-storage.md)
  for the full reference and conformance coverage at
  `conformance/tests/stdlib/oauth_storage.harn`.
- **Durable agent channel emits (#1871).** Adds `emit_channel(name, payload,
  options?)` with tenant-default scope resolution, session/pipeline/tenant
  prefixes, signed `emitted_at` timestamps, and idempotent `options.id`
  handling backed by the event log. A `channel_events(...)` inspection helper
  covers conformance and local diagnostics; org-scoped channels resolve but
  return `HARN-CHN-002` until org grants land.
- **Lifecycle span links for suspend/resume and drain scaffolding (#1858).**
  VM tracing now has `suspension`, `resume`, `drain`, and
  `drain_decision` span kinds plus causal span links. Worker suspension
  snapshots persist `prior_span_link` plus the closed pipeline span link
  so cold resumes can link the fresh resume span to closed lifecycle spans
  instead of parenting across process boundaries. The OTel helper layer
  now exposes `set_span_link` beside `set_span_parent`, with no-op
  behavior in non-OTel builds.
- **Resume-continuity system reminder provider (#1845).**
  `agent_loop(...)` now fires the canonical `resume_continuity` provider
  before the first turn after `resume_agent(...)`. The one-shot reminder
  identifies the suspension turn and reason, resume cause, optional
  resumer input, and the pre-suspend digest when resuming with
  `continue_transcript: false`; it is dedupe-keyed, non-propagating, and
  honors the standard reminder provider opt-out list.
- **Reminder lifecycle telemetry and ACP updates (#1828).** Reminder
  injections, render-time firings, dedupe replacements, TTL/compaction/
  clear expiry, malformed drops, sub-agent inheritance, and provider
  evaluation now emit `transcript.reminder.*` EventLog records with
  `session_id`, `task_id`, and `agent_id` correlation fields. Rendered
  reminders also surface through ACP `session/update` as
  `reminder_emitted` under `_meta.harn.reminder`, and hook-origin
  reminder metadata is carried on tool-call receipts.

### Changed

- **Natural-language tool binder defaults reflect the empirical run
  envelope (#1696).** `with_natural_language_executor` now defaults to a
  `500ms` wall-clock budget and `1024` response tokens. The larger token
  budget gives reasoning binders room to emit structured JSON after their
  reasoning preamble; the timeout remains a bounded opt-in hop and still
  degrades to passthrough on overrun.

### Fixed

- **`harness.system.platform()` mock arch leak.** The mock-mode
  `platform()` snapshot in `vm/methods/harness.rs` was reading the host
  CPU architecture via `std::env::consts::ARCH` instead of a fixed
  placeholder, so the `harness/system_basic` conformance fixture passed
  on the Apple Silicon dev machines that recorded it (`aarch64`) but
  failed on the x86_64 Linux CI runners. Mock mode now returns a
  deterministic `"arch": "mock"` matching the rest of the snapshot
  (`os`, `kernel`, `hostname`, …), so the fixture is portable across
  architectures. The real (non-mock) `platform_snapshot()` still
  returns the live host arch.

## v0.8.26

### Added

- **Sub-agent reminder propagation via handoff envelopes (#1824).**
  Handoff artifacts and sub-agent requests now carry
  `reminder_propagation` beside `policy_override`; `handoff(...)` derives
  it from the active session's pending reminders when omitted. Child
  sessions seed inherited reminders before their first turn, rewrite
  `source` to `"inherited"`, and retain `originating_agent_id` for
  audit. `propagate: "all"` continues through descendants,
  `propagate: "session"` stops after direct children, and
  `propagate: "none"` remains local. Canonical `idle_nudge` and
  `tool_output_truncated` reminders now default to `propagate: "none"`.
- **Agent lifecycle tools for self-parking and parent resume control (#1840).**
  `agent_loop(...)` now registers `agent_await_resumption(reason,
  conditions?)` on every loop and handles it structurally when the loop is
  running inside a worker, reusing the worker suspend checkpoint path and
  `parse_resume_conditions(...)`. Parent loops can opt into
  `subagent_pause(handle, reason)` and `subagent_resume(handle, input?,
  continue_transcript?)` with `subagents: true`, `subagent_tools: true`, or
  `agent_lifecycle_tools(registry, {subagents: true})`; lifecycle tool calls
  emit `tool_call_audit` metadata with the initiator and reason.
- **`with_audit_log` local sink emits a `file://` `receipt_uri` (#1693).**
  When `with_audit_log({sink: "local"})` or `"both"` is active, the
  middleware now attaches a `file://` URI pointing at
  `.harn/receipts/<session_id>.jsonl` to both the in-process
  `result.audit` envelope and the typed `ToolCallReceipt`'s embedded
  `audit` dict. The portal's RunDetail view already rendered an
  `audit.receipt_uri` deep-link when present; with this change the link
  resolves to the persisted JSONL for any session that opts into the
  local sink. Cloud-only sinks omit the URI (nothing wrote to disk).
  Closes #1693.
- **`harn run <bundle.harnpack>` verify + replay + execute (#1784).**
  Closes the `.harnpack` epic (#1779) by wiring the `harn run` command
  to accept a signed content-addressed bundle. Detection is by
  `.harnpack` extension or zstd magic header; the runner verifies the
  embedded Ed25519 signature through the OpenTrustGraph trust store,
  re-derives the bundle hash, checks `harn_version` compatibility
  (patch mismatch warns, minor/major refuses), replays the archive into
  `$HARN_CACHE_DIR/packs/<bundle_hash>/` (atomic, so concurrent runs
  share a cache slot), and executes the manifest entrypoint with
  `Harness::real()`. Unsigned bundles are refused by default; pass
  `--allow-unsigned` to override (intended for local development).
  `--dry-run-verify` performs verification and replay without executing,
  useful for deployment gates. A new `pack_run` event flows through the
  `harn run --json` NDJSON stream carrying the bundle hash, signature
  outcome, signing key id, cache-hit status, and dry-run flag so agents
  can audit pack execution without rereading the archive.
- **Typed effect inheritance on `HandoffArtifact` (E5.3, #1776).** Adds
  `EffectRecord { kind, scope, resource }` with `EffectKind` (`Stdio`,
  `Fs`, `Net`, `Llm { provider, model }`, `Tool { name }`,
  `Hostcall { name }`, `Persona { id }`, `Spawn`) and `EffectScope`
  (`Read`, `Write`, `Mutate`, `Observe`) and wires an
  `effects: Vec<EffectRecord>` field onto `HandoffArtifact` envelopes.
  A new `compute_handoff_effects(source, ceiling)` utility reuses the
  same capability analysis that backs `harn graph --json` (#1758) and
  walks the AST for harness sub-handle method calls
  (`harness.net.get`, `harness.fs.write_file`, …) that the IR doesn't
  see directly. `attach_spawn_handoff_effects` and the
  `handoff_effects(source, ceiling?)` Harn builtin let spawn shims
  populate the field at child-spawn time, clamping the computed set
  to the spawn-config declared ceiling. Enforcement of the ⊆ relation
  lands in E5.4 (`HARN-CAP-301`); the OpenTrustGraph receipt-chain
  embedding lands in E5.5.
- **`owned<T>` + scope-exit auto-drop (#1789).** A new `owned<T>` type
  modifier marks a binding as carrying sole ownership of a drop-able stdlib
  handle (channels, sync permits, future MCP/ACP/file handles). The compiler
  emits an implicit `defer { drop(<binding>) }` at the binding's enclosing
  block so the resource closes deterministically on normal exit, early
  `return`, `break` / `continue`, and uncaught throws — no manual
  `close_channel` / `mcp_disconnect`. `drop()` is a new builtin that
  dispatches on the runtime value tag; unknown values are a silent no-op so
  callers can hand it any value. The existing `defer { ... }` block is now
  also block-scoped (it previously leaked to function scope), running at the
  end of its innermost `{ ... }` and at each loop-body iteration. A new
  `channel_is_closed(channel) -> bool` builtin makes channel lifecycle
  observable from tests. `HARN-OWN-003` (`OwnershipEscape`) warns when an
  `owned<T>` binding escapes via `return` without the function declaring
  `owned<T>` as its return type; widening the return type signals an
  explicit ownership transfer.
- **Transcript reminder transforms (#1817).** Adds
  `transcript.inject_reminder(transcript, options)` and
  `transcript.clear_reminders(transcript, selector)` over pending
  `system_reminder` events. Injection validates reminder options with
  `HARN-RMD-001`, appends a typed reminder event, replaces older
  pending reminders with the same `dedupe_key`, and emits
  `transcript.reminder.deduped` when an EventLog is active. Agent
  post-turn processing now decrements finite `ttl_turns`, removes
  expired reminders, and emits `transcript.reminder.expired`.
- **Hook-emitted system reminders (#1819).** Tool, persona, step, and
  session hooks can now return typed reminder effects using
  `{reminder: {body, tags?, dedupe_key?, ttl_turns?,
  preserve_on_compact?, propagate?, role_hint?}, then?}`, a bare
  reminder spec, or a session-level effect list. Hook reminders inject
  into the active session transcript as `system_reminder` events, use
  `source: "hook"`, honor `dedupe_key`, and compose with existing
  allow/deny/modify/block/decision return shapes. `register_tool_hook`
  also accepts `pre` and `post` closures alongside the legacy `deny`
  and `max_output` shortcuts.
- **Reminder provider registry (#1820).** `agent_loop(...)` now enables
  canonical stdlib reminder providers for token pressure, daemon idle
  nudges, truncated tool output, and post-compaction recaps. Loop callers
  can opt out with `reminders: {providers: ["-token_pressure"]}` or
  disable providers with `reminders: false`; bare `llm_call(...)` does
  not auto-fire providers. `register_reminder_provider({id,
  subscribes_to, evaluate})` lets Harn scripts add provider callbacks
  that return the same reminder effects used by hooks, and
  `clear_reminder_providers()` clears user-defined providers for tests
  and isolated runs.
- **Bridge `session/remind` reminder injection (#1821).** Bridge and ACP
  hosts can queue typed system reminders with the same
  `interrupt_immediate` / `finish_step` / `wait_for_completion` delivery
  modes as queued user messages. `session/input`, `user_message`, and
  `agent/user_message` remain user-role only; reminders validate the
  reminder-spec payload, report malformed payloads as `HARN-RMD-002`, and
  enter the transcript with `source: "bridge"`.
  reminder-spec payload and enter the transcript with `source: "bridge"`.
- **Capability-aware system-reminder rendering (#1822).** Pending
  `system_reminder` events now render at LLM-call time based on the
  resolved provider capability row: OpenAI developer-role routes receive
  separate `developer` messages, Anthropic user-block hints prepend a
  `<system-reminder>` content block with prompt-cache metadata when
  supported, Gemini XML routes fold reminders into the system prompt with
  XML scaffolding, and local fallback routes use plain system text. `harn
  lint` emits `HARN-RMD-003` when a pipeline hardcodes
  `role_hint: "user_block"` alongside a route that cannot preserve that
  shape.
- **Compaction-aware reminder lifecycle (#1823).** `transcript_compact`
  now treats pending `system_reminder` events as first-class lifecycle
  state: finite `ttl_turns` values are decremented at the pre-compaction
  boundary, expired reminders emit `transcript.reminder.expired`,
  duplicate `dedupe_key` reminders collapse to the newest event, and only
  `preserve_on_compact: true` reminders are copied verbatim into the
  compacted transcript. Custom compactors receive surviving reminder
  payloads as a second closure argument, and `harn lint` emits
  `HARN-RMD-004` for discardable reminder literals with no finite TTL.
- **OAuth provider catalogue records (#1905).** Adds
  `std/oauth/providers` with ten preconfigured OAuth provider records
  (GitHub, Slack, Linear, Notion, Google, Microsoft, Atlassian,
  Discord, GitLab, Bitbucket), the `github_enterprise(base_url, ...)`
  and `custom(config, ...)` factories, and override-friendly endpoint
  fields for future OAuth orchestration work. The catalogue is static
  and offline-only; it does not start a live OAuth flow.
- **Pipeline `on_finish` callback + finish lifecycle hooks (#1854).**
  Adds the `pipeline_on_finish(callback)` builtin and three new
  session-level hook events — `PreFinish`, `PostFinish`, and
  `OnUnsettledDetected` — emitted by `Vm::execute` after the script's
  declared steps complete. The callback receives `(harness,
  return_value)` and its return value replaces the pipeline's return
  value, so post-completion logic (transcript wrap-up, audit emission,
  settlement decisions) has a typed boundary instead of an abandoned
  trailing edge. `std/lifecycle` exports `on_finish_abandon` (the
  legacy no-op preset) and `on_finish_drain` (a stub the P-02
  settlement-agent epic replaces). `OnUnsettledDetected` only fires
  when a future unsettled-state snapshot is non-empty; P-01 ships an
  always-empty snapshot so the wiring is exercised without depending
  on the suspend/resume, trigger-queue, handoff-envelope, or
  in-flight-LLM-call producers that follow under harn#1853. Foundation
  ticket for the Pipeline Lifecycle Framework epic.
- **Tool-hook catalogue primitive: `tool_rule`, `catalogue`,
  `tool_hooks_registry`, `tool_hooks_register`, `tool_hooks_unregister`,
  `tool_hooks_list`, `tool_hooks_match` (#1894).** Foundational TH-01
  schema + registry for the preset tool-hooks epic (#1884). `ToolRule`
  values carry an id, regex-or-callable pattern, `applies_to` stack list,
  severity, optional rewrite closure, explanation, references, and
  priority. `Catalogue` bundles rules with stack/version/source
  provenance, and the registry composes catalogues with predictable
  ordering. `tool_hooks_match` sweeps catalogues × rules linearly and
  sorts results by rule priority, catalogue priority, then declaration
  order so downstream mode callbacks (TH-03) and the
  `preset_run_command` wrapper (TH-02) layer cleanly on top.
- **Harness unsettled-state snapshot foundation (#1857).** Adds
  `harness.unsettled_state()`, `harness.is_empty(state?)`,
  `harness.counts(state?)`, and `harness.summary(state?)`, plus matching
  `std/lifecycle` helpers for `on_finish` callbacks. Suspended subagents
  and in-flight LLM calls now enumerate from live VM registries; trigger
  and handoff buckets are stable typed empty lists until their per-item
  registries land. Root harness action methods are present, with worker
  resume/cancel delegated to host worker primitives and not-yet-backed
  actions returning typed unsupported results.
- **Named agent thread pools — `std/lifecycle/pool` (#1886).** Adds
  `pool_create({name, max_concurrent, ...})` plus per-handle
  `pool.submit(closure, options?)`, `pool.size()`, and `pool.snapshot()`
  for bounding concurrent agent work behind a named, queue-backed pool.
  Submissions accept `key` and `priority` hints (higher priority dequeues
  first; ties FIFO). `pool_wait(handle)` and `wait_agent(handle)` both
  block on pool task handles. Foundation for the agent pool epic
  (#1883); queue strategies, backpressure, durability, channel
  composition, and OTel spans land in #1887..#1893.
- **Agent pool queue strategies (#1887).** Extends
  `std/lifecycle/pool` with `fifo()`, `priority()`, `lifo()`,
  `fair_round_robin(key?)`, and the `QueueStrategy()` namespace helper.
  `pool_create({queue: ...})` now selects deterministic FIFO, LIFO,
  priority, or key-partitioned fair round-robin dispatch while preserving
  priority dispatch as the default for existing pools.
- **Agent pool backpressure policies (#1888).** Adds
  `Backpressure()` plus `backpressure_queue(max_depth, on_full)`,
  `fail_fast()`, and `ring_buffer(capacity)` descriptors for bounding pool
  queues. Full queues can block submitters, drop oldest/newest tasks,
  fail immediately with `HARN-POL-001`, or reject fail-fast submissions
  with `HARN-POL-002`; drop paths return rejected task handles and emit
  `pool_drop` audit events on `lifecycle.pool.audit`.
- **Per-symbol stdlib metadata contract (#1790).** Every public stdlib
  function declares an `@effects`, `@allocation`, `@errors`,
  `@api_stability`, and `@example` block above its `pub fn`. A new
  `harn_parser::stdlib_metadata` module parses those fields, LSP hover
  surfaces them as a markdown panel beneath the existing doc comment,
  `harn graph --json` exposes the parsed block on each `public_symbols`
  entry, and the `HARN-STD-101` lint (`missing-stdlib-metadata`) gates
  the embedded `crates/harn-stdlib/src/stdlib/` tree so new public
  surfaces cannot land without a declared contract. `make lint-harn`
  blocks the release gate on any HARN-STD-101 warning. Coverage at
  landing: 803 of 864 public stdlib functions (92.9%); the remaining
  61 functions are exempt because their preceding doc block is missing
  (separately tracked by HARN-LNT-024).
- **`harn check --json` and `harn fmt --json` (#1759).** Adds
  standard `JsonEnvelope` reports for static checks and formatter
  runs, including per-file status, diagnostics, summary counts, and
  schema catalog entries. Provider and connector matrix JSON output
  now uses the same envelope; `--format=json` remains as a deprecated
  alias for the matrix commands for one patch release. Merge Captain
  ladder, iterate, and audit commands now accept `--json` with
  `--format=json` kept as a deprecated alias.
- **`harn parse --json` and `harn tokens --json` (#1757).** Adds
  parser and lexer inspection commands for tooling. `harn parse` prints
  the AST in text mode or a tagged `Program` JSON tree with byte spans
  in `--json` mode. `harn tokens` prints the lexer stream in text mode
  or `{ kind, lexeme, start, end, line, column }` entries in `--json`
  mode. Both structured surfaces use the standard `JsonEnvelope` and
  register in `harn --json-schemas`.
- **LSP repair metadata for quick fixes (#1750).** Harn LSP diagnostics now
  include flat `data.code`, `data.repair_id`, and `data.safety` fields
  alongside the existing nested repair envelope. Repair-backed code actions use
  `quickfix.harn.<safety>` kinds and carry `{ repair_id, safety,
  diagnostic_code }` data so IDE clients can dispatch without parsing
  diagnostic prose.
- **Diagnostic-code catalog: generated `docs/src/diagnostics.md` +
  `docs/diagnostics-catalog.json` sidecar (#1751).** New
  `harn explain --catalog [--format markdown|json|text]` dumps the
  in-binary `HARN-<CAT>-<NNN>` registry as a complete catalog so agents,
  editors, and hosted error pages dispatch on `schemaVersion: 1`
  metadata (`code`, `category`, `summary`, `repairs[]`, `related[]`,
  `explanationPresent`, `apiStability`) without parsing prose. The mdBook
  catalog page is regenerated via `make sync-diagnostics-catalog`; a new
  `make check-diagnostics-catalog` CI gate fails on drift between the
  committed catalog and the in-binary registry. The hand-written tour of
  shape diagnostics moved to `docs/src/reading-shape-diagnostics.md`.
- **`harn test conformance --json` conformance report (#1756).** The
  conformance runner now emits a versioned `JsonEnvelope` with a stable
  `snapshotKey`, per-fixture outcomes, summary counts, diagnostic-code
  extraction, and machine-enforced `@xfail` handling where stale markers
  surface as `xfail_unexpected_pass` failures.

## v0.8.25

### Added

- **`harn time run [--json]` — phase-timed run with cache hit/miss
  and per-LLM/tool latency (#1787).** New top-level `harn time`
  subcommand wraps an existing subcommand and emits a structured
  timing breakdown. `harn time run script.harn` prints a human
  table; `harn time run script.harn --json` emits a versioned
  `JsonEnvelope` with five fixed phases (`parse`, `typecheck`,
  `bytecode_compile`, `run_setup`, `run_main`), a `cache: "hit" |
  "miss"` marker on `bytecode_compile`, per-LLM-call entries
  (model, latency, tokens), per-tool-call entries (name, latency),
  and totals (`wall_ms`, `cpu_ms`, `cache_hits`, `cache_misses`).
  The envelope is registered under `time run` in `harn
  --json-schemas`. Pair with `--no-cache` to force a cold compile
  and `-e <code>` to time an inline script. Script `stdout` is
  routed to stderr in `--json` mode so the envelope is alone on
  stdout for `jq`-style consumption.
- **`harn doctor --json` capability matrix (#1785).** Doctor now emits a
  `JsonEnvelope` (`schemaVersion: 2`) whose `data` carries four structured
  sections in addition to the existing `checks` / `hardware` / `summary`
  keys consumed by Burin Code preflight: `host` (os, arch, harn version,
  rust toolchain), `targets[]` (per-Rustup triple `installed` +
  `buildable` flag, with reasons), `providers[]` (per-provider
  `configured` / `reachable` / `latency_ms` / `errors`), and
  `capabilities[]` (each stdlib effect mapped to the sandbox profiles
  that permit it on this host). `summary` adds the spec-aligned
  `blocking` / `warning` counts. New opt-in flags `--check-providers`
  and `--check-targets` fan out active HTTP / `cargo check` probes
  (parallel-fanned for providers; only configured providers are probed
  so no-credential rows don't pollute the report with 0ms
  `unreachable`). The legacy `--no-network` flag is removed — doctor is
  now offline-clean by default, so the flag was redundant. Text output
  is regenerated from the same `DoctorReport`, so JSON and human
  surfaces never drift.

- **`harn dev --watch` incremental loop gated by interface fingerprints
  (#1786).** New `harn dev --watch [<root>]` watches every `.harn` file
  under `<root>` (defaulting to the cwd) and re-type-checks only the
  modules whose public surface actually moved. Each module carries a
  BLAKE3 **interface fingerprint** over its public types, function /
  pipeline / tool signatures, struct + enum shapes, and `pub import`
  re-exports — bodies, doc comments, and private helpers are
  intentionally excluded. An edit that flips a fingerprint transitively
  invalidates importers via `ModuleGraph::importers_of`; an edit that
  leaves the fingerprint stable only re-checks the changed file.
  `--json` emits an NDJSON event stream (`ready` /
  `fingerprint_changed` / `rerun` / `diagnostics` / `tests`) wrapped in
  the canonical `JsonEnvelope`, registered under `dev` in
  `harn --json-schemas`. `--with-tests` extends the loop to also
  re-run `test_*` / `@test`-attributed pipelines in each invalidated
  module. New `harn_modules::fingerprint` module exposes
  `fingerprint_program` / `fingerprint_file` / `fingerprint_source` for
  reuse by editors and the bytecode cache.
- **`harn routes <root> [--json]` static trigger inventory (#1788).** New
  top-level command audits declarative trigger projects without executing
  handlers, reporting route paths, handler modules, declared budgets, inferred
  host capabilities, vendor-lock disclosure, and template framework-overhead
  tokens. The JSON mode uses the standard `{ schemaVersion, ok, data, error,
  warnings }` envelope and is registered in `harn --json-schemas`.
- **`harn explain HARN-<CAT>-<NNN>` text + `--json` envelope (#1748).** The
  `explain` subcommand now dispatches on registered stable diagnostic codes
  in addition to its original control-flow invariant form. `harn explain
  HARN-TYP-014` prints an embedded markdown explanation plus a curated
  `See also:` list; `harn explain HARN-TYP-014 --json` emits a stable
  envelope `{ schemaVersion, code, category, summary, body, repairs,
  related, apiStability }` that LSPs, IDEs, hosted error-page mirrors,
  and agents can dispatch on without parsing prose. Every entry in the
  diagnostic-code registry (#1746) carries an explanation markdown body
  under `crates/harn-parser/src/diagnostic_codes/explanations/HARN-<CAT>-
  <NNN>.md`, embedded via `include_str!` so a missing file fails the
  build — the CI gate required by the epic. The legacy
  `harn explain --invariant <NAME> <FUNCTION> <FILE>` form continues to
  work; both forms share one dispatcher.
- **`harn run --json` event stream (#1755).** Adds a versioned NDJSON
  surface to `harn run`: one `JsonEnvelope` per line, each carrying a
  typed `RunEvent` with a strictly monotonic `seq`. Variants cover
  `stdout`, `stderr`, `transcript`, `tool_call`, `tool_result`, `hook`,
  `persona_stage`, plus a terminal `result` / `error` event. `--quiet`
  drops the noisy `stdout` / `stderr` events while keeping the
  structured stream intact. The `run` command is now listed in
  `harn --json-schemas` with `schemaVersion: 1`. Foundation for the
  `--json` epic (#1753) and the burin-code TUI replatform.
- **`llm_call` mid-stream `output_schema` abort (#1775).** When an
  `llm_call` carries `output_schema` and `schema_stream_abort` is on
  (the new default for schema-bearing calls), the streaming transport
  feeds every visible text delta through the incremental validator from
  `std/json/stream`. The first delta that makes the partial JSON unable
  to satisfy the schema short-circuits the provider stream, emits a
  `schema_stream_aborted` transcript event (`provider`, `model`,
  `reason`, `path`, `chunks_consumed`), increments
  `harn_llm_schema_stream_aborted_total{provider,model}`, and surfaces
  an `ErrorCategory::SchemaStreamAborted` to the schema-retry loop.
  The loop consumes one `schema_retries` slot per abort and folds the
  abort `path` + `reason` into the corrective nudge, so the next
  attempt gets a sharper prompt than a generic stream failure. Opt out
  by passing `schema_stream_abort: false` to let the stream complete
  and rely on `schema_retries` for post-hoc recovery. SSE
  (`consume_sse_lines`), NDJSON (`consume_ollama_ndjson_lines`), and
  the in-process `FakeLlmProvider` all route through the shared
  `StreamSchemaWatch` helper so script-driven and test-driven paths
  exercise the same logic.
- **`harn skills list / get / dump` over the embedded corpus (#1762).**
  Wires the `harn-skills` bundled corpus (#1761) through the CLI:
  `harn skills list [--json]` returns the canonical Harn skills shipped
  with this build, `harn skills get <name> [--full] [--json]` prints
  one skill's frontmatter (and SKILL.md body when `--full` is passed),
  and `harn skills dump --all [--out <dir>] [--force]` writes every
  skill to disk as byte-stable copies of the embedded source. `--json`
  output uses the canonical `JsonEnvelope` shape from #1754, with a
  `skill_not_found` error envelope and `error.details.available`
  payload on unknown lookups. The legacy FS-discovery list view moves
  to `harn skills resolved` (same flags as the old `list`), and
  `harn skills list / get` register in the `harn --json-schemas`
  catalog as schema-version 1.
- **Repair classifier on diagnostics (#1747).** `TypeDiagnostic` grows a
  structured `repair: Option<Repair>` field carrying a namespaced repair
  id (`bindings/make-mutable`, `imports/fix-path`, …), a one-line
  summary, and a six-level `RepairSafety` taxonomy
  (`format-only` → `behavior-preserving` → `scope-local` →
  `surface-changing` → `capability-changing` → `needs-human`). Agents
  and IDEs dispatch on `repair.safety` to decide whether to auto-apply,
  propose, or escalate. The catalog maps ≥20 diagnostic codes to
  templates via `Code::repair_template()`; `LintDiagnostic` and
  `FormatError` expose the same handle through derived
  `.repair()` methods. The CLI now renders a `repair: ID [SAFETY] —
  SUMMARY` line, and the LSP attaches the same payload to
  `Diagnostic.data` as `{"repair": {"id", "summary", "safety"}}`.
- **`harn pack <entrypoint>` builds a `.harnpack` run bundle (#1781).**
  New top-level CLI subcommand that walks the entrypoint's transitive
  imports, precompiles every module into the bytecode-cache header
  format, snapshots the provider catalog hash + stdlib pin, generates a
  minimal SBOM (one entry per module + stdlib dep), and assembles a v2
  `WorkflowBundle` manifest. The result is a deterministic tar.zst
  archive written to `<entrypoint>.harnpack` (overridable with
  `--out <path>`). `--json` emits the canonical `JsonEnvelope`
  containing `bundle_hash` (BLAKE3 over the canonical manifest + sorted
  content hashes), `output_path`, `size_bytes`, and the full manifest;
  the catalog row is registered with `harn --json-schemas`.
  `--upgrade <old.harnpack>` reads an older bundle (v1 JSON or v2
  archive) and re-emits it under the v2 manifest, preserving the prior
  bundle's id, name, version, triggers, workflow graph, and prompt
  capsules while populating the new v2 fields from the entrypoint walk.
  `--sign --key <path>` signs the canonical bundle hash with an
  Ed25519 PKCS#8 PEM key, embeds the manifest signature, and emits an
  OpenTrustGraph `release` record; `--unsigned` skips the manifest
  signature but still emits the release record at autonomy tier
  `suggest`. Full SBOM enumeration (E6.4) lands in a follow-up.
  Companion: `harn-vm` exposes
  `read_workflow_bundle_manifest_any_version` and
  `load_workflow_bundle_any_version` for relaxed-schema reads, and
  `bytecode_cache::serialize_chunk_artifact` /
  `serialize_module_artifact` so in-memory consumers can package
  bytecode bytes byte-identical to the on-disk `.harnbc`/`.harnmod`
  files.
- **`Harness` capability handle + `main(harness: Harness)` entrypoint
  (#1766).** First step of the E4 epic that replaces ambient stdio /
  clock / fs / env / random / net globals with an explicit handle threaded
  through `main`. Introduces the public `harn_vm::Harness` Rust type and
  its six sub-handles (`HarnessStdio`, `HarnessClock`, `HarnessFs`,
  `HarnessEnv`, `HarnessRandom`, `HarnessNet`), each carrying the same
  refcounted `HarnessInner` and exposed at the Harn-language level via
  field access (`harness.stdio`, `harness.clock`, etc.). `Harness::real()`
  wires `harn-clock::RealClock` as the production backing; the
  filesystem, environment, random, and network surfaces are stubbed and
  land alongside the ambient-builtin migrations in E4.2-E4.4. The VM
  auto-invokes `fn main` when present with the `harness` global set by
  the run path (CLI, conformance, bench, playground), and the
  typechecker enforces the new convention via `HARN-NAM-101`: any
  `fn main` with a signature other than `main(harness: Harness)` or the
  unused-capability opt-out `main(_harness: Harness)` is rejected at
  parse time. `harness.stdio.println` / `print` / `eprintln` / `eprint`
  and `harness.clock.now_ms` / `monotonic_ms` / `sleep_ms` are wired
  end-to-end so `examples/hello_v2.harn` runs against the new shape
  today.
- **`harn fix --plan --json` repair plans (#1749).** Adds plan-only
  `harn fix` mode for diagnostics with registered repair classifiers.
  `harn fix --plan <path>` prints a summary table and
  `harn fix --plan --json <path>` emits a `RepairPlan` with
  `schemaVersion: 1`, `diagnostics[]`, `repairs[]`, and
  `safetyLevels[]`. Plan mode mirrors the check path across type-check,
  lint, and preflight diagnostics, reports concrete `FixEdit` entries
  when available, marks overlapping edits via `applies_cleanly: false`
  plus `conflicts_with`, and never writes files. `--safety <class>`
  filters proposals by maximum autonomy class.
- **`harn fix --apply --safety <class>` guarded repair application
  (#1752).** Extends `harn fix` with the apply half of the repair
  contract. Apply mode requires an explicit ceiling from `format-only`
  through `capability-changing`, rejects `needs-human` as propose-only,
  filters out conflicts and above-ceiling repairs, writes accepted
  `FixEdit`s bottom-up per file, then reruns the same diagnostic passes
  to report `post_apply_diagnostics_count`. `--dry-run` reports the same
  apply set without writing, and `--apply --json` emits a
  schema-versioned result with `applied[]`, `skipped[]`, skip reasons,
  and the post-apply diagnostic count. The `harn --json-schemas` catalog
  now includes `fix apply`.
- **`with_scoped_executor` tool-caller middleware (#1702).** Narrows the
  active `CapabilityPolicy` for the duration of one tool dispatch:
  `compose_tool_callers([..., with_scoped_executor({stage, allowed_tools,
  side_effect_level?, capabilities?}), ...])`. The middleware pushes the
  scoped policy via `with_execution_policy(...)` (now intersected with
  the ambient policy so it can only tighten the surface, never widen
  it), preemptively rejects out-of-allowlist tool names with the new
  `status: "scope_violation"`, and decorates results with
  `audit.scope = {stage, allowed_tools, ...}`. Pairs with
  `PersonaRuntimeBinding.stages` for per-stage tool scoping outside a
  persona manifest. Opt-in `on_violation: "raise"` throws instead of
  short-circuiting so `try { agent_loop(...) }` can route on the
  exception.
- **In-turn parallel tool dispatch (#1699).** `agent_loop` now accepts a
  `max_concurrent_tools: N` option (default 1). When a planner emits
  multiple tool calls in a single turn, siblings dispatch concurrently
  capped at `N`; middleware-backed calls use `parallel settle`, and the
  host batch path honors the same cap. Each middleware sibling invokes
  its own caller chain in a fresh scope so `audit.layers` histories
  never cross-talk between concurrent calls. Results inject in source
  order regardless of completion order so text tool-call parsers keyed
  on declaration order keep working. The tool-call envelope grows
  `turn.tool_call_index`, and typed `ToolCallReceipt` records grow an
  `emit_order` field equal to that index so completion-ordered receipts
  can be re-sorted to source order. The new `prefetch_next_turn: true`
  option starts the next planner turn after tool results are recorded
  while local/custom audit receipt sinks flush in the background; the
  loop drains those flushes before returning. Parallel tool dispatch
  now scopes `current_tool_call_id()` task-locally across `.await`
  points and propagates cancellation instead of turning it into an
  ordinary tool error.
- **Natural-language tool binder middleware (#1698).** New experimental
  `std/llm/tool_binder` module exporting
  `with_natural_language_executor({provider, model, timeout_ms, ...})`.
  Composes via `compose_tool_callers` to intercept a tool call, hand
  the planner-emitted intent + JSON Schema to a sub-100ms binder LLM
  (Cerebras GPT-OSS-120B is the primary substrate), and replace
  `tool_args` with the binder's structured output before dispatch.
  OFF by default — activation requires explicit composition. Hard
  wall-clock budget per the parent epic (#1696): overruns drop the
  binder hop and pass through unchanged with
  `audit.binder.status = "timeout"`. Surfaces a typed audit slot at
  `audit.binder = {provider, model, status, latency_ms, reason?, attempts}`
  alongside the existing `with_audit_log` receipt machinery.
- **Per-tool-call FS snapshot + restore hostlib primitives (#1720).**
  Adds `hostlib_fs_snapshot`, `hostlib_fs_restore`,
  `hostlib_fs_list_snapshots`, and `hostlib_fs_drop_snapshot` builtins
  under the existing `fs/` schema bucket. Snapshots are keyed by the
  caller-supplied `scope_id` (canonically the ACP `toolCallId`) and
  stored content-addressed under
  `.harn/state/snapshots/<session>/<scope>/`. `tools/write_file` and
  `tools/delete_file` lazy-capture pre-images into any open snapshot
  bound to the current `harn_vm::agent_sessions::current_tool_call_id`,
  so a single `hostlib_fs_snapshot({session_id, scope_id})`
  registration is enough to roll the next mutation back. Session
  bundles evict oldest-first past a configurable byte cap (default
  1 GiB).
- **ACP `session/restore_tool_call` method (#1720).** The ACP server now
  advertises `{ sessionCapabilities: { restoreToolCall: {} } }` in the
  initialize response and exposes a `session/restore_tool_call` method
  that drives `harn_hostlib::fs_snapshot::restore`. Each restore emits
  a canonical `session/update` with `sessionUpdate: "tool_call_update"`,
  `status: "restored"`, and `_meta.harn.kind = "tool_call_restored"`.

### Changed

- **`harn eval tool-calls` binder schema works under Cerebras strict mode
  (#1698 follow-up).** The binder's JSON Schema response shape used
  `arguments: {"type": "object"}` with no `properties` list, which
  Cerebras's strict OpenAI-compat structured-output endpoint rejects with
  HTTP 400 (`Object fields require at least one of: 'properties' or
  'anyOf'`). The harness now asks the binder for a JSON-stringified
  `arguments_json` field with all top-level fields unconditionally
  required (strict OpenAI-compat providers also reject conditional
  `if/then/else` requirement schemas), and the scorer parses the string
  back. The legacy inline `arguments` object shape is still accepted as a
  fallback so non-strict providers keep working. This unblocked the
  empirical go/no-go cell against Cerebras GPT-OSS-120B for #1698.
- **`llm_call` now recognizes `timeout_ms` (#1698 follow-up).** The
  option surface gains `timeout_ms` as a first-class alias for
  `timeout` so `with_timeout`-style middleware, the natural-language
  tool binder, and other callers that already think in milliseconds
  stop having their value silently dropped at the parse layer. The
  underlying HTTP transports still consume `Duration::from_secs(u64)`,
  so a `timeout_ms` value is rounded UP to the nearest whole second
  for the network-level backstop; sub-second budgets must additionally
  be enforced at the caller (the binder middleware already does this
  via a wall-clock post-check). When both `timeout` and `timeout_ms`
  are set, the explicit `timeout` (seconds) wins.
- **WorkflowBundle schema v2 / `.harnpack` foundation (#1780).** Portable
  workflow bundles now use schema v2 with canonical package metadata
  (`entrypoint`, transitive module hashes, Harn/stdlib versions, provider
  catalog hash, tool manifest, SBOM, signature slot, and parent trust record
  linkage). The VM can build/read deterministic `.harnpack` `tar.zst`
  archives with `harnpack.json` at the root, computes BLAKE3 bundle hashes
  over canonical manifest bytes plus sorted content hashes, and rejects v1
  manifests with a typed schema-version error.
- **Dependency refresh.** Bulk dependency audit and bump across the
  workspace. Notable Cargo bumps: `jsonwebtoken` 9 → 10 (now selects
  the `aws_lc_rs` crypto backend explicitly to match the rustls
  provider already in use), `zip` 2 → 8 (the `flate2` Cargo feature is
  folded into `deflate-flate2`), `sha1` 0.10 → 0.11 and `sha3` 0.11 →
  0.12 to align with the rest of the RustCrypto family. Workspace
  `cargo update` rolled minor/patch upgrades for aws-lc, hyper-rustls,
  jsonschema, rustls-pki-types, tokio, tonic, tower-http, wasm-bindgen,
  and the wasm-* tool crates. npm: portal jumps `vitest` 3 → 4 and
  bumps `react-router-dom`, `react-intl`, `@codemirror/view`, `eslint`,
  `vite`, and `typescript-eslint` to current. Root `@redocly/cli` and
  vscode `@vscode/vsce` bumped to clear high/moderate `npm audit`
  vulnerabilities in their transitive trees (`protobufjs`,
  `fast-xml-builder`, `fast-uri`). All workspaces now report
  `0 vulnerabilities`.

## v0.8.24

### Fixed

- **Swift protocol artifact CaseIterable synthesis.** Generated Swift
  wire-value enums now include explicit `allCases` arrays so deprecated
  cases such as ACP `session/stop` do not break downstream Swift builds.

## v0.8.23

### Added

- **Staged filesystem hostlib mode (#1722).** Adds the `fs/` hostlib
  capability with `hostlib_fs_set_mode`, `hostlib_fs_staged_status`,
  `hostlib_fs_commit_staged`, and `hostlib_fs_discard_staged`.
  Sessions can switch to deferred writes stored under
  `.harn/state/staged/<session_id>/`; hostlib file reads, directory
  listings, outlines, AST parse-file calls, and code-index file reads
  see pending changes before disk, while commit/discard controls apply
  or drop the accumulated diff. ACP hosts can drive the same mode with
  `session/fs_mode` and `session/fs_commit_staged`, and receive
  `session/update` progress notifications tagged
  `_meta.harn.kind = "staged_writes_pending"`.
- **Imported-module bytecode cache + correctness fixes (#1710 follow-up).**
  Extends the v0.8.22 entry cache to imported modules — both stdlib and
  user files now persist a `.harnmod` artifact alongside the entry
  `.harnbc`, eliminating per-process re-parse and re-compile of every
  imported function pool. `harn precompile` emits both artifact families
  per source so shipped libraries hit the cache whether the user runs
  them as an entry or imports them. Cache key now folds in active
  `CompilerOptions` (so flipping `HARN_DISABLE_OPTIMIZATIONS` between
  runs no longer reuses a chunk compiled under the opposite setting),
  and the on-disk format gains a kind discriminant so the two artifact
  families coexist in one cache dir without filename collisions. Header
  schema bumped to `2`; older v0.8.22-written cache files are rejected
  fail-closed and recompiled.
- **MCP client notification relay.** The MCP client transport now
  routes inbound `notifications/progress`, `notifications/message`,
  and `*/list_changed` server-to-client notifications into the
  originating session's `agent_inbox` (kinds `mcp_progress`,
  `mcp_log`, `mcp_resource_change`). Correlation is by per-call
  progress token: every outgoing `tools/call` registers a fresh token
  with the new `client_progress` registry and injects it as
  `_meta.progressToken`; inbound notifications carrying that token
  push to the issuing session even when the transport reader runs on
  a different task. Previously these notifications were silently
  dropped at the stdio read loop.
- **`agent_session_post_event(id, kind, content, source?)` builtin.**
  Triggers, connectors, and external host integrations can now post
  events directly into a running session's inbox — the canonical way
  to wire "GitHub PR you've been waiting on just merged" or "your
  remote build finished" nudges into a mid-loop agent. Paired with
  `agent_session_drain_inbox(id)` for explicit drain in scripted
  control flow. Both available at the host-builtin layer
  (`__host_agent_session_post_event`) and as Harn-side wrappers in
  `std/agent/state`.

### Changed

- **Unified per-session agent inbox.** Folded the cross-thread
  `GLOBAL_PENDING_FEEDBACK` queue, its paired `Condvar`, and the
  bespoke push/drain/wait surface into a single per-session inbox in
  `crate::orchestration::agent_inbox`. Producers (long-running tool
  workers, command-policy post-hooks, MCP server notifications,
  triggers, host-side `run_command` background output) call
  `agent_inbox::push(session_id, kind, content, source)` with a typed
  source label; consumers drain in FIFO order with monotonic
  per-session sequence numbers. Synchronous callers still get a
  Condvar-backed `wait_sync`; async callers get the new clock-aware
  `wait_async(session_id, timeout, &dyn Clock)` that composes with
  `harn_clock::PausedClock`. The legacy
  `push_pending_feedback_global` / `drain_global_pending_feedback` /
  `wait_for_global_pending_feedback` exports were removed — breaking
  change for out-of-tree consumers, who should switch to the new API
  one-to-one.
- **Compaction-time event delivery race fixed.** `std/agent/loop` now
  drains the agent inbox **before** `agent_autocompact_if_needed` so
  the summary reflects events that landed between turns, and drains
  **again afterwards** so any push that landed during a Tier-2 LLM
  summarization call (typically 5–30 s wall-clock) is injected into
  this turn's prompt instead of waiting an extra turn. Closes a
  silent-loss class for tool completions, MCP progress
  notifications, and trigger-driven nudges that arrived mid-
  compaction.

### Fixed

- **`wait_async` cross-thread notify race.** The per-session
  `agent_inbox::wait_async` waiter was checking `pending_count`
  *before* creating its `tokio::sync::Notify::notified()` snapshot.
  A producer thread that completed its entire `push` (entry append +
  `notify_waiters`) between those two steps left the new `Notified`
  with a counter snapshot already at the post-increment value, so the
  await would park indefinitely even though an entry was sitting in
  the queue. The waiter now captures the `Notified` first and re-
  checks `pending_count` against it: any push that completed before
  the snapshot is visible via the entry count, and any push that
  completes after the snapshot triggers the `Notified`.
- **ACP now exposes upstream `session/close` (#1725).** The stdio and
  WebSocket ACP adapters advertise `sessionCapabilities.close`, route
  `session/close` through session cleanup and active-prompt cancellation,
  and keep `session/stop` as a one-release deprecated alias with a warning.
  Generated TypeScript and Swift protocol bindings expose `session/close`
  as the canonical method and mark `session/stop` deprecated.

## v0.8.22

### Fixed

- **`.githooks/` cover the same incremental-cache corruption #1707
  fixed in the release scripts.** `release_gate.sh` and
  `release_ship.sh` export `CARGO_INCREMENTAL=0` for the prepare
  phase, but `.githooks/pre-commit` and `.githooks/pre-push` run
  cargo from fresh subprocesses the export does not reach — committing
  the "Release vX.Y.Z" change still hit `cached cgu ... should have an
  object file, but doesn't` against the stale incremental dir from the
  audit. A new helper in `lib.sh`
  (`hook_disable_cargo_incremental_if_release_bump`) inspects the
  staged/push diff for a `^version =` change in `Cargo.toml` or
  `crates/*/Cargo.toml`; on a real workspace bump it exports
  `CARGO_INCREMENTAL=0` and purges `target/debug/incremental` for the
  hook run only. Day-to-day commits keep incremental cache enabled.
- **`release_ship.sh --prepare` auto-renames `## Unreleased` →
  `## vX.Y.Z` instead of erroring after the audit.** The accumulator
  convention between releases is to drop new bullets under
  `## Unreleased`; the strict heading check in
  `require_changelog_top_matches` rejected that exact shape and only
  surfaced after burning ~7 min of audit wall time. The check now
  promotes a top `## Unreleased` heading to `## v$expected` in place
  (subsequent `git add -u` in the staging step picks the rename up).
  Mismatched `## vX.Y.Z` headings still error with the same hint.
- **Lint: for-loop iteration bindings no longer trigger
  `undefined-function` false positives (#1704).** `for entry in
  callables { entry(arg) }` is now valid — the iteration binding is
  tracked as a callable local. Affects only the linter; runtime
  behavior is unchanged.

### Added

- **Bytecode cache + `harn precompile` (#1710).** `harn run` now persists
  compiled bytecode under `$HARN_CACHE_DIR` (defaulting to
  `~/.cache/harn/bytecode`) and reloads it whenever the entry source and
  every transitively-imported user file are unchanged. Header-stable
  format records magic, schema version, harn version, source hash, and
  import-graph hash; mismatches transparently fall back to recompile.
  `harn precompile <path>` precomputes artifacts for shipping
  pre-compiled `.harnbc` files adjacent to source — the loader picks
  those up before consulting the shared cache. Cold-start cost on small
  pipelines collapses from full parse+typecheck+compile to a single
  `fopen` + `bincode::deserialize`. Gates the burin-code thin-rust
  cutover at burin-code#814. See `docs/perf/bytecode-cache.md`.
- **`secret_store` hostlib capability (#1714).** Adds
  `hostlib_secret_store_{get,set,delete,list}` to `harn-hostlib`, a
  generic per-application credential primitive any Harn-hosted
  application can compose. Picks an Apple Keychain backend on
  macOS/iOS, a Credential Manager backend on Windows, and a
  `0o600`-permissioned JSON file at
  `$XDG_CONFIG_HOME/<account>/credentials.json` everywhere else.
  `HARN_SECRET_STORE_BACKEND=file` forces the file backend on every OS
  for sandboxed CI and eval harnesses. The file-backend path layout is
  byte-compatible with burin-code's existing credentials file so
  existing deployments migrate without data movement, and JSON schemas
  under `crates/harn-hostlib/schemas/secret_store/` keep `harn check`
  preflight covering the new builtins. Docs at
  `docs/src/hostlib/secret_store.md`.
- **ACP `session/set_config_option(configId="model")` (#1721).**
  Generalized the config-option dispatch table from a hardcoded
  `mode`-only branch to a registry covering `mode` and `model`.
  Pinning a model writes a per-session selector that the LLM resolver
  (`vm_resolve_model` / `vm_resolve_provider`) honours for subsequent
  prompts without touching the transcript, tool-call audit log, or
  memory context. Per-call `model:` options still win, and clearing
  the pin (`value: "@inherit"`) reverts to the ambient default. Closes
  the editor `/model` workaround that hinted "takes effect after
  `/clear`" — the new wire path matches the mid-session model swap
  Crush / OpenCode / Codex already ship.
- **Cerebras provider (#1705).** Added a first-class `cerebras` provider
  routed through the OpenAI-compatible stack with a `provider_family =
  "openai"` capability inheritance row. Catalog ships canonical
  `gpt-oss-120b`, `llama-3.3-70b`, and `qwen-3-coder-480b` model entries
  (wire IDs match Cerebras's `/v1/models`); selectors of the form
  `cerebras/<model>` route to the provider and normalize to the bare
  wire ID. `scripts/smoke_cerebras.harn` is the latency-floor smoke for
  the NL-binder substrate work tracked under #1696/#1698 — empirical
  warm-state p50 of ~150ms from a residential network, p99 ~230ms.
- **Tool-call telemetry: standardized spans + built-in Langfuse / OTel /
  stderr / noop sinks (#1704).** `with_telemetry` (in
  `std/llm/tool_middleware`) now builds a standardized tool-call span
  for every dispatch — including timing, args hash, executor, layered
  child spans, and `dispatched` / `result_returned` /
  `scope_violation` events — and fans it out to one or more sinks.
  The new `std/llm/tool_telemetry` module exposes
  `tool_call_span(call, result, start_ms, end_ms, extras?)` plus
  `langfuse_sink`, `otel_sink`, `stderr_sink`, `noop_sink`, and
  `resolve_telemetry_sink` for custom middleware. `with_telemetry`
  accepts a callable, a built-in name string, or a config dict
  (`{sink: "langfuse", project: "..."}` /
  `{sinks: [...]}` for fan-out). The Langfuse sink POSTs to
  `${LANGFUSE_BASE_URL}/api/public/ingestion` using the standard
  `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` env shape, with
  per-span `on_error` opt-in. Captain presets renamed
  `telemetry_sink` → `telemetry` to take the new config shape
  directly. Schema reference: `docs/src/observability/tool-call-spans.md`.

### Removed

- **`prefer_prefill_done` model field.** Dropped the vestigial flag from
  `ModelDef`, the `model_info(...)` builtin dict, the generated provider
  catalog (Rust struct + JSON schema + JSON / TS / Swift artifacts), and
  the two `providers.toml` rows (`gemma-4-e2b-it`, `gemma-4-e4b-it`)
  that set it. The flag's only consumer in burin-code now gates on the
  per-model `supports_assistant_prefill` capability added in #1665
  (burin-labs/burin-code#787), so the field carried no behavior.

## v0.8.21

### Added

- **Portable `harn.context_artifact.v1` envelope in `std/context`
  (#1687).** Host-neutral envelope carries kind / scope / language /
  applicability / freshness / confidence / provenance / source-hash /
  token / redaction metadata, plus ranking, dedupe, merge, and
  budget/select helpers. Provider-capability-aware rendering covers
  Markdown / XML / plain / compact shapes, and a Burin digest adapter
  preserves `.burin/context-digests` compatibility. Docs and
  conformance coverage land alongside the envelope so downstream
  IDE/eval hosts can pin against a stable shape.
- **Prompt context-quality gates for `harn eval prompt` (#1682).**
  `harn eval prompt` now accepts repeatable `--context-fixture <json>`
  inputs. Each fixture case supplies candidate repository artifacts plus
  `assemble_context` options and expectations for selected artifact ids,
  stale/noisy rejection, token budget adherence, and logical-section
  envelopes across the fleet. JSON/HTML/terminal output includes a
  `context_eval` report with per-case scores and per-model section
  shapes, giving Burin CI/eval dashboards a deterministic gate for
  context-engineering changes before live prompt runs.
- **Context maintenance hook recipes (#1681).** Session hooks now include
  `post_turn`, and `std/context/maintenance` provides the portable
  `harn.context_maintenance.job_receipt.v1` shape for non-blocking
  `context.refresh` / `context.crystallize` jobs. The new docs and
  `examples/triggers/context-maintenance` package cover file-edited,
  session-idle, pre-compact, post-turn, and session-end scheduling,
  plus deterministic replay include/skip behavior.

### Changed

- **LLM provider/model catalog defaults now live in an embedded
  `crates/harn-vm/src/llm/providers.toml` (#1692).** Roughly 1,100
  lines of Rust literals in `default_config()` collapse into a single
  TOML asset that deserializes through the same `ProvidersConfig`
  schema already used by `HARN_PROVIDERS_CONFIG`,
  `~/.config/harn/providers.toml`, `harn.toml [llm]`, and package
  manifests — one source of truth. The catalog refresh registers
  `claude-sonnet-4-5/4-6/4-7`, `claude-opus-4-6/4-7`,
  `qwen/qwen3-coder`, `deepseek/deepseek-v3.2`,
  `moonshotai/kimi-k2.6`, `openai/gpt-oss-120b`, and the
  OpenRouter-routed `google/gemini-2.5-flash`; flags
  `claude-sonnet-4-20250514` (sunset 2026-06-15),
  `claude-sonnet-4-5` (2026-05-15), `claude-opus-4` (2026-06-15),
  `claude-opus-4-1`, `gpt-4o` (API sunset 2026-02-17), and
  `gpt-4-turbo` as deprecated with replacement notes; and bumps the
  `frontier` tier alias from `claude-sonnet-4-20250514` to
  `claude-sonnet-4-6`. Brittle per-value assertions are replaced by
  invariant tests (`llm_config::tests::embedded_*`) that verify the
  TOML parses, every deprecated model carries a `deprecation_note`,
  every alias/model targets a known provider, every `qc_default`
  resolves, pricing rates stay non-negative, and the
  `frontier`/`mid`/`small` tier aliases all resolve to active catalog
  entries.

### Fixed

- **Release binary artifact publishing is more robust (#1691).** The
  macOS release binary job now retries transient Apple notary
  polling/upload failures, the complete expected release asset set is
  validated before checksumming/uploading, and `publish-release` no
  longer creates a visible GitHub release before binary assets are
  ready — `build-release-binaries` owns release notes and assets so
  the GitHub release flips public only once binaries are attached.
- **Release `--prepare` no longer corrupts cargo incremental cache
  (#1707).** Audit lanes populate `target/debug/incremental/` keyed to
  the pre-bump crate hashes; once `bump_version` rewrites
  `Cargo.toml`, the workspace rebuilds with fresh hashes and the
  stale incremental directory leaves dangling `.o` references that
  abort `make gen-protocol-artifacts` mid-build. `release_gate.sh
  cmd_prepare` and `release_ship.sh prepare_here` now export
  `CARGO_INCREMENTAL=0` for the prepare phase (covering both the
  inner cargo invocations and the fresh shell that runs
  `regenerate_derived_files`). The Ctrl-C first-signal message is
  also expanded to "[harn] signal received, interrupting VM (give it
  a moment to unwind in-flight async ops; Ctrl-C again to
  force-exit)..." so operators don't reach for a second Ctrl-C
  during the brief cancel-grace window.

### Internal

- **Long Harn modules split into focused files (#1690).** Trigger
  event core types, payloads, catalog, normalization, utilities, and
  tests now live under `triggers/event/`. The LLM façade is split
  into call execution, agent host primitives, and
  stream/mock/trace-builtin glue while preserving existing
  re-exports. `harn connect` is split into OAuth, status, store,
  callback, GitHub, Linear, workspace, and test modules, and
  reflected OAuth callback HTML is now escaped on the way out.

## v0.8.20

### Fixed

- **`if`-without-`else` expression value matches its static type.** The
  VM compiler emitted the falsy-branch `Pop; Nil` cleanup at the end of
  the truthy branch too, so `let x = if true { 42 }` left `nil` on the
  stack instead of `42`. The truthy path now jumps past the cleanup
  unconditionally. The type checker is also corrected to infer the
  expression as `T | nil` (rather than just `T`) when there is no
  `else` arm, so the compile-time type now agrees with the runtime
  value — `let x: int = if cond { 1 }` is rejected, and `int?` /
  `?? default` are the supported recoveries.
- **`type_of(x)` narrows through parameterised types.** The flow
  narrower only recognised `Named("list")` / `Named("dict")` /
  `Named("closure")` etc. as members of a union, so
  `list<int> | int` refused to narrow on `type_of(x) == "list"` and
  the falsy branch retained the original union. `narrow_to_single` and
  `remove_from_union` now treat `List(_)`, `DictType` / `Shape`,
  `FnType`, `Iter(_)`, `Generator(_)`, `Stream(_)`, and literal
  refinements as the corresponding runtime kind. `generator`, `stream`,
  and `iter` are added to the known-typeof set, and a single
  non-union variable also participates in the narrowing.
- **Closure return types are inferred in a scope with their params.**
  `{ x: int -> x + 1 }` previously failed return-type inference because
  the body referenced `x`, which was not bound when the closure
  literal was probed for its `fn(...)` shape. The closure then
  collapsed to the opaque `closure` type, which was compatible with
  any `fn(...)` slot, so `let g: fn(int) -> string = { x: int -> x + 1 }`
  silently type-checked. The inference call now happens inside a
  child scope populated with the declared param types.
- **`else if` chains survive `harn fmt` inside expressions.** Only the
  top-level statement formatter unwound a nested `IfElse` in an
  `else` body back into `else if`. The expression-position formatters
  (closure bodies, `let x = if … else if …`, function-call arguments)
  did not, so each `else if` round-tripped through `fmt` as
  `else { if … }` and gained an indent level. The expression formatter
  now matches the statement formatter.
- **`match` with a wildcard arm counts as a definite exit.** When every
  arm of a `match` ends in `return` / `throw` / `break` / `continue`
  and one arm is an unguarded `_ -> { ... }`, the lint and type
  checker now treat code after the `match` as unreachable. Previously
  `match x { _ -> { return 0 } }` did not satisfy the exit-detection
  predicate, so trailing statements escaped the dead-code lint and the
  type checker refused to refine the surrounding flow as `never`.
- **`session_idle` lifecycle hook now fires.** `register_session_hook("session_idle", …)`
  was registrable but had no firing path, so handlers never ran. The
  daemon-mode agent loop now fires it once per `wake_interval_ms` wait
  with `{session, iteration, wake_interval_ms, consolidate_on_idle}`,
  matching the placement documented in `docs/src/extensibility/hooks.md`.
  Conformance coverage at `conformance/tests/hooks_session/session_idle.harn`.
- **LLM provider behavior dispatch now uses capabilities.** Rust call
  sites that previously branched on provider strings for Anthropic-style
  messages, native tool schemas, image URL support, reasoning request
  shapes, and file-upload transports now read `provider_capabilities`.
  Bedrock-hosted Claude and custom proxy routes can opt into the same
  behavior through the capability matrix instead of pretending to be the
  canonical provider name.
- **Trailing comments on body statements are preserved.** `harn fmt`
  previously dropped same-line comments that followed a statement inside
  any block body (`fn`/`pipeline`/`if`/`else`/`while`/`for`/`try`/`catch`/
  `finally`/`match` arms), shoving them to the end of the surrounding
  scope or, worst-case, to end-of-file. The formatter now splices the
  trailing `// …` or `/* … */` back onto the source line where it was
  written. Idempotent across repeated formatting passes.
- **Multi-line ternary expressions parse correctly.** Wrapping a ternary
  with `?` or `:` at either the end of the previous line or the start of
  the next line is now accepted in all three layouts (`cond ?\n a : b`,
  `cond\n ? a\n : b`, `cond ? a\n : b`). Previously the parser
  misclassified `?` at the end of a line as a postfix-try operator,
  producing confusing `expected separator (\`;\` or newline), found :`
  errors.
- **Arity errors pluralize correctly.** "Builtin function 'len' expects 1
  argument, got 0" instead of "expects 1 arguments". The runtime
  `ArityMismatch` similarly drops the awkward `argument(s)` rendering in
  favor of conditional `argument`/`arguments`.
- **Statement spans no longer absorb trailing blank lines.** `let`/`var`
  bindings, assignments, and `if`/`try`-`catch` statements whose body is
  followed by blank lines and standalone comments previously reported a
  `span.end_line` that walked past those blank lines. The formatter's
  trailing-comment splicer was then attaching the standalone comments
  onto the closing brace. A new `last_non_newline_span()` parser helper
  keeps node end-spans pinned to the last significant token.

### Added

- **`template.render` transcript events for variant resolution (#1668).**
  Every `render()` / `render_prompt()` call performed under an
  LLM-aware frame now emits a `template.render` event into the run's
  `llm_transcript.jsonl`. The event captures the resolved `llm` snapshot
  (`provider`, `model`, `family`, `capabilities`), a per-`{{ if }}`
  /`{{ elif }}` /`{{ section }}` branch trace with line + column
  anchors, the template URI + content hash, and the rendered byte count.
  The portal surfaces a new "Variant resolution" panel showing which
  capability branches fired for the active model, so an on-call engineer
  can answer "which prompt did this model actually see?" without
  re-running the script. Renders outside any LLM frame (doc-gen, CI)
  emit no event. The branch trace is deterministic across repeated
  renders of the same template + bindings, which is what makes replay
  reproducible.
- **`template-provider-identity-branch` lint rule (#1669).** `harn lint`
  now also walks `.harn.prompt` (and bare `.prompt`) files and warns
  when a template branches on `llm.provider`, `llm.model`, or
  `llm.family` directly. Vendor-identity strings are the trap every
  prompt-variant framework before #1663 fell into — the diagnostic
  carries a per-comparison capability-flag replacement suggestion
  (e.g. `provider == "anthropic"` →
  `llm.capabilities.prefers_xml_scaffolding`).
- **`template-variant-explosion` lint rule (#1669).** Companion rule
  that warns when a single `.harn.prompt` carries more than `N`
  capability-aware conditionals — combinatorial explosion + drift
  between materializations is the #1 failure mode for prompt-variant
  systems per the literature. Default threshold is 3, configurable via
  `[lint] template_variant_branch_threshold = N` in `harn.toml`.
- **`harn eval prompt <file> --fleet <models>` (#1670).** New CLI
  subcommand that renders a single `.harn.prompt` template against a
  fleet of models so authors can validate that capability-adapted
  envelopes stay equivalent across the provider mix. Three modes:
  `--mode render` (the default) pushes a per-model
  `LlmRenderContext` and resolves the template — no LLM calls,
  byte-deterministic across runs. `--mode run` extends render by
  invoking each model and collecting outputs; unauthenticated providers
  are skipped with a warning (override with `--fail-on-unauthorized`).
  `--mode judge` adds a final LLM-as-judge call (default
  `claude-opus-4-7`, override with `--judge-template` /
  `--judge-model`). Output via `--output terminal|json|html` and
  optionally redirected with `-o <path>`. Fleets accept ad-hoc
  comma-separated selectors (`alias` or `provider:model`) or named
  groups declared as `[eval.fleets.<name>]` in `harn.toml` via
  `--fleet-name`. Run / judge modes synthesize a thin Harn driver and
  reuse the existing `llm_call` pipeline so credentials, the provider
  catalog, and `HARN_LLM_PROVIDER=mock` work exactly as in `harn run`.
- **Provider format-preference capabilities (#1665).**
  `provider_capabilities(...)`, `llm_model_info(...)`, the provider
  catalog, and the generated provider matrix now expose prompt-format
  preferences separately from transport feature gates: XML vs. Markdown
  section scaffolding, native JSON vs. delimited/XML-tagged output
  preference, assistant prefill support, developer-role instructions,
  XML text-tool rendering, and thinking-block style. The shipped matrix
  is populated for Anthropic, OpenAI/Azure, Gemini/Vertex, Qwen/local
  routes, Ollama, and Bedrock so prompt renderers can adapt without
  provider-name dispatch. This bumps the generated provider catalog
  schema to v2 because model entries now require `format_preferences`.
- **Capability-aware prompt templates: auto-injected `llm` scope (#1664).**
  When `render()` / `render_prompt()` / `render_string()` is invoked
  from inside an LLM-aware frame (`llm_call`, `default_llm_caller`,
  `agent_loop`), the engine now auto-injects a reserved
  `llm = {provider, model, family, capabilities: {...}}` binding so a
  single logical `.harn.prompt` can branch on
  `llm.capabilities.native_tools` and friends without manual option
  threading. Bare `render()` calls outside any LLM frame leave
  `llm = nil`; templates guard with `{{ if llm }}` for the CI / doc-gen
  path. User bindings that already supply an `llm` key win for
  back-compat and trigger a one-shot `template.llm_scope` lint warning.
  This is the load-bearing primitive for the capability-adaptive prompt-
  rendering epic (#1663) — sibling tickets layer logical sections,
  format-preference capabilities, and cross-model eval on top.
- **Logical prompt-template sections (#1666).** `.harn.prompt` assets
  now support `{{ section "task" }} ... {{ endsection }}` and the
  built-in logical roles `task`, `examples`, `output_format`, `tools`,
  `thinking_scaffold`, `chain_of_thought`, and `system_framing`.
  Rendering is driven by provider capability flags such as
  `prefers_xml_scaffolding`, `prefers_markdown_scaffolding`,
  `structured_output_mode`, `prefers_xml_tools`, and
  `thinking_block_style`, so a single logical prompt can materialize as
  Claude XML, GPT-style Markdown/native JSON, local text-tool
  instructions, or a generic fallback without provider-string branches.
- **One-line installer + signed binaries.** `install.sh` (served from
  `harnlang.com/install.sh`) now detects the host OS/CPU, resolves the
  latest GitHub release tag, downloads the matching pre-built archive,
  and verifies it against the new `SHA256SUMS` release asset before
  extracting `harn`, `harn-dap`, and `harn-lsp`. The destination is
  picked from `$HARN_INSTALL_DIR`, `$XDG_BIN_DIR`, `$HOME/bin`,
  `$HOME/.local/bin`, or `$HOME/.harn/bin` in that order — so the
  default install path no longer requires `sudo`. macOS binaries
  remain signed + notarized via the existing release workflow, and
  Linux/Windows tarballs/zips are now hash-pinned end-to-end.
- **`harn upgrade` subcommand.** Resolves the latest GitHub release
  (or a specific `--version vX.Y.Z`), downloads the matching archive,
  verifies it against `SHA256SUMS`, and atomically replaces the
  currently running binary in-place. Use `--check` to print the
  resolved versions without downloading, `--force` to reinstall the
  same version, and `--no-verify` only as an escape hatch.
- **`harn demo` subcommand for cold-start aha (#1650).** A new
  top-level CLI command ships three bundled, fully-offline scenarios
  that demonstrate the Harn moat — `merge-captain` (persona-supervised
  PR triage with structured receipts), `review-captain` (HITL
  clarifying-question loop), and `provider-race` (latency-aware
  provider racing with cost attribution). Each scenario embeds a
  `.harn` script and a JSONL `--llm-mock` tape into the binary, so
  `harn demo merge-captain` finishes in well under a second on a fresh
  install with zero API keys, zero network, and zero project setup.
  `harn demo --list` prints the menu; `harn demo --json` emits a
  machine-readable summary; `harn demo <id> --live` opts into the
  configured provider. The README `Quick Start` now leads with the
  demo. CI exercises every scenario via the new `demo_cli`
  integration test, which catches drift between the script and tape.
- **First-class `routing_policy` primitive (#1649).** `routing_policy({...})`
  builds a reusable handle that drives a chain of providers with
  failover, latency-aware racing, and per-call / session budget caps.
  Pipe it through `llm_call(... routing: policy ...)`:

  ```harn,ignore
  let policy = routing_policy({
    chain: [
      {provider: "anthropic", model: "claude-opus-4-20250514"},
      {provider: "openai",    model: "gpt-4o"},
      {provider: "ollama",    model: "llama4:70b"},
    ],
    failover: {on_status: [429, 500, 502, 503, 504], max_attempts: 3},
    latency: {race_after_ms: 5000},
    budget:  {per_call_usd: 0.5, on_exceed: "abort"},
    observe: {emit_event: "billing.routing_decision"},
  })

  let result = llm_call("Summarize this PR.", nil, {routing: policy})
  ```

  Each chain attempt rides on the result envelope's `routing.attempts`
  block and emits structured tape events
  (`<dispatch>.{decision,attempt,race_started,race_won,race_lost,budget_exceeded,exhausted}`)
  so transcripts and replay can attribute every outcome to a specific
  link. Migrates the previous BYO composition of `with_routing` +
  `with_retry` + `with_fallback` to a single typed primitive while
  reusing the catalog pricing in `std/llm/economics` for budget
  enforcement. See `docs/llm/harn-quickref.md` and
  `docs/src/stdlib/llm-handlers.md`.
- **Production OS sandbox profiles (#1647).** `CapabilityPolicy`
  now carries a `sandbox_profile: SandboxProfile` field
  (`unrestricted` / `worktree` / `os_hardened` / `wasi`). The
  default `worktree` matches today's behavior — workspace-root path
  enforcement plus best-effort OS confinement. Pipelines that run
  untrusted code opt into `os_hardened`, which makes the OS
  confinement *required* (Linux Landlock + seccomp, macOS
  sandbox-exec, Windows AppContainer + Job Object): spawns return
  `tool_rejected` if the platform mechanism is unavailable,
  regardless of `HARN_HANDLER_SANDBOX`. The `host_call("process",
  "exec", ...)` host call accepts `sandbox_profile: "..."` to
  promote a single spawn without rewriting the surrounding policy.
  Three Harn builtins surface backend identity for diagnostics:
  `sandbox_active_backend()`, `sandbox_backend_available()`, and
  `sandbox_active_profile()`. The 1.2k-line `stdlib/sandbox.rs`
  was split into a `stdlib/sandbox/` directory with one
  `SandboxBackend` impl per OS, so callers no longer branch on
  `cfg(target_os)` and adding a backend is one new file. Full
  per-platform capability → kernel-knob mapping table lives in
  [docs/src/sandboxing.md](docs/src/sandboxing.md).
- **Session-level lifecycle hooks (#1648).** `register_session_hook(event, handler)`
  registers callbacks for the whole-session turn lifecycle:
  `session_start`, `session_end`, `user_prompt_submit`, `pre_compact`,
  `post_compact`, `permission_asked`, `permission_replied`,
  `file_edited`, `session_error`, and `session_idle`. The
  `user_prompt_submit` and `pre_compact` events accept
  `{block: true, reason}` to veto the operation;
  `permission_asked` additionally accepts
  `{decision: "allow"|"deny"|"ask", reason}` to short-circuit the
  dynamic permission policy. Hook invocations are captured on the
  active session transcript under `hook_call`, `hook_returned`, and
  `hook_vetoed` event kinds, so replay tooling reproduces the same
  control flow. `write_file`, `append_file`, and `write_file_bytes`
  queue `file_edited` notifications automatically (drained at each
  turn boundary); call `notify_file_edited(path, metadata?)` to emit
  one explicitly. Manifest `[[hooks]]` entries accept the new event
  names. See `docs/src/extensibility/hooks.md`.

## v0.8.19

### Changed

- **`provider_capabilities` exposes text-tool capability.** The dict
  returned by `provider_capabilities(...)` and `llm_model_info(...)` now
  includes `text_tool_wire_format_supported` (mirrors the rule field) and a
  derived `tools` boolean (`native_tools || text_tool_wire_format_supported`)
  that matches the VM's own tool-capability gate at
  `effective_model_capability_tags`. Scripts gating on "can this model do
  tools" should prefer `caps.tools` — gating on `caps.native_tools` alone
  was rejecting tool-capable local routes like
  `ollama/qwen3.6:35b-a3b-coding-nvfp4` that use Harn's text wire format.
- **`tool_choice` accepted on text-format tool routes.** The VM's
  capability gate rejected `tool_choice` whenever `caps.native_tools`
  was false, even for routes whose text-format tool wire is fully
  supported. Agent scripts that pass `tool_choice: "none"` to suppress
  further tool calls (e.g. release_harn's finalization turn) hit
  "option `tool_choice` is not supported by ..." mid-loop. The gate
  now permits tool_choice whenever either `native_tools` or
  `text_tool_wire_format_supported` is true, mirroring the relaxed
  `tools` gate.
- **`agent_preset` derives `tool_format` from the model capability matrix.**
  Tool-using preset kinds (`audit`, `repair`, `merge_captain`,
  `review_captain`, …) used to hardcode `tool_format: "native"`, which the
  VM rejected with "option `tools` is not supported by ..." for tool-capable
  text-format routes (e.g. `ollama/qwen3.6:35b-a3b-coding-nvfp4`) before
  `native_tool_fallback` could engage. The preset now calls
  `llm_resolve_model` to resolve `tool_format` from the capability matrix,
  picking `"native"` for native-capable routes and `"text"` for text-only
  routes. Callers that explicitly set `tool_format` still win.

## v0.8.18

### Added

- **Agent progress reporting (#1628).** `std/agent/progress` adds
  `agent_progress(...)`, which emits a structured `progress_reported`
  agent event for the current session with optional narration, task-list
  entries, replacement semantics, and metadata. `agent_loop` keeps the
  model-facing tool off by default; opt in with `progress_tool: true` or
  customize `{name, description, system_prompt_nudge}`. ACP extension
  notifications and protocol artifacts now include the new event kind.
- **ACP progress plan routing (#1627).** ACP renders `agent_progress`
  entries as canonical `sessionUpdate: "plan"` updates so clients replace
  their structured task list directly, while message-only progress reports
  continue to use Harn's vendor `progress` extension with `phase:
  "narration"`.
- **A2A progress status updates (#1629).** A2A task streams now translate
  `progress_reported` events into non-terminal `working` status updates
  with agent message text. Entry lists render as deterministic markdown
  checklists, while terminal completion status emission remains unchanged.
- **Provider telemetry envelope (#1614).** `llm_call` results now carry a
  normalized `provider_telemetry` block that preserves server-side timings
  local runtimes already report and represents missing fields explicitly.
  Ollama's `/api/chat` NDJSON and `/api/generate` raw stream lift
  `total_duration`, `load_duration`, `prompt_eval_duration`, and
  `eval_duration` (rounded to milliseconds) plus the prompt-eval / eval
  token counts and the daemon-resolved model id. OpenAI-compatible
  responses expose `prompt_tokens` / `completion_tokens` even when no
  durations are reported, and llama.cpp's `usage.timings` extension is
  promoted to `llamacpp_timings` with the prefill / decode breakdown.
  Anthropic Messages responses preserve the response id alongside usage
  counts. Every call also records `client_wall_ms` so end-to-end latency
  can be decomposed from the network and streaming overhead the
  server-side counters omit. The same envelope flows through the
  structured-output wrapper so eval aggregators get the data without
  provider-specific decoders. The `OllamaPsModel` parser is shared with
  `harn local list` / `status`, picking up `context_length` from the
  `/api/ps` payload where the daemon reports it.
- **`harn provider probe <provider>` command (#1614).** Machine-readable
  one-shot snapshot of provider readiness plus the loaded-model state
  Ollama surfaces via `/api/ps` (size, VRAM, expiry, context window).
  JSON output by default for eval pipelines; pair with `harn local
  status --json` to inspect lifecycle state across every local runtime.
- **Done-judge cadence policy (#1631).** `agent_loop` now accepts
  `done_judge.cadence` with `every`, `when`, `max_invocations`, and
  `min_iterations_before_first` gates so completion checks can run on
  explicit cadence or stall signals instead of every completion
  candidate. Accepted judge calls still use transcript projection
  isolation: judge prompts and structured responses emit events but do
  not mutate the worker session transcript.
- **Stall-gated done judge (#1632).** `agent_loop` now runs a configured
  `done_judge.cadence.when: "stalled"` judge when stall diagnostics emit
  `agent_loop_stall_warning`. A `done` verdict stops the loop with
  `stalled_done_judge` before repeating the stalled tool call; a
  `continue` verdict falls back to the existing stall feedback path.
  Judge events now include an optional `trigger`, set to `"stalled"` for
  stall-fired calls.

## v0.8.17

### Added

- **`harn local` runtime lifecycle commands (#1599).** New top-level
  command surface for managing local LLM runtimes: `harn local list`
  surveys every local provider Harn knows about (Ollama, llama.cpp, MLX,
  generic OpenAI-compatible, vLLM) with base URL, port, served models,
  loaded models, and memory footprint; `harn local status` shows the
  currently-active selection plus a brief per-provider summary;
  `harn local switch <alias>` warms a model on its provider, evicts
  conflicting local runtimes (drains Ollama's loaded set, stops tracked
  llama.cpp/MLX PIDs), re-checks `/v1/models` after the first success,
  and persists the selection to `<state>/local/selection.json`;
  `harn local stop [--all]` unloads loaded Ollama models via
  `keep_alive=0` and SIGTERMs Harn-managed llama.cpp/MLX PIDs.
  Defaults for `--ctx` and `--keep-alive` come from a machine profile
  derived from RAM and accelerator presence so a 48 GB Apple Silicon
  laptop picks a wider context window than a low-RAM Linux box.

## v0.8.16

### Added

- **`HarnWorkerStatus` protocol artifact (#1604).** `dump-protocol-artifacts`
  now emits the canonical worker-status wire vocabulary
  (`running`, `progressed`, `awaiting_input`, `completed`, `failed`,
  `cancelled`) as a typed enum in every binding (TypeScript, Swift,
  Python, Go) and a `harnWorkerStatuses` field on the manifest. Hosts
  consuming `worker_lineage[].status` or `worker_update` payloads can now
  vendor this enum instead of maintaining hand-rolled synonym buckets
  for statuses Harn never emits (e.g. `succeeded`, `error`, `timed_out`).
  `WorkerEvent::ALL` exposes the same set programmatically.

## v0.8.15

### Added

- **Current local coding model aliases and setup guidance (#1601).** Catalog
  now ships aliases for Qwen3.6 35B A3B Coding (NVFP4), Gemma 4 26B MoE,
  Devstral Small 2 24B, llama.cpp Qwen3.6 GGUF, and Apple MLX Qwen3.6 27B.
  Local Qwen3.6/Devstral routes default to the text-tool contract (with
  `*-native` siblings kept for targeted parser A/B tests). `harn quickstart`,
  `harn models recommend`, and `harn models install` prefer/present the new
  local coding models and now print concrete setup plans for llama.cpp,
  MLX, and generic local OpenAI-compatible servers. Documented local
  provider setup and known MLX server flag variance.
- **`/api/ps` observability in model-info (#1600).** `harn model-info
  <alias> --verify` (and `--warm`) now hits `/api/ps`, surfaces the
  loaded runner's `context_length`, and reports the `num_ctx` Harn would
  request as `expected.num_ctx`. When the two diverge, `context_drift`
  explains the load-time semantics and points at `ollama stop <model>`
  for recovery. Also documented in `docs/src/llm/providers.md`.

### Fixed

- **Ollama warmup now applies `num_ctx` (#1600).** The `ollama_readiness`
  warmup path previously sent `/api/generate` with only `keep_alive` set,
  which caused Ollama to load the runner at the model's declared maximum
  context (e.g. `262144` for `qwen3.6:35b-a3b-coding-nvfp4`) regardless of
  `HARN_OLLAMA_NUM_CTX` or the catalog's `runtime_context_window`. The
  warmup body now derives from `OllamaRuntimeSettings::warmup_body`, so
  the runner is loaded at the same `num_ctx` chat/completion paths
  request.

## v0.8.14

### Added

- **Local/A2A dispatch replay proof (#1586).** Trigger dispatch now records
  trust-boundary and remote identity metadata on action-graph nodes, outbox
  success records, and TrustGraph terminal records. A2A dispatch also
  classifies cleartext/authority denials, timeouts, incompatible response
  schemas, and remote `rejected` task states distinctly, with a new
  `examples/triggers/local-a2a-dispatch` reference flow and replay-oracle
  fixture proving local and A2A handlers can preserve logical output.
- **Replay determinism benchmark harness (#1585).** Added
  `harn bench replay`, a canonical replay benchmark suite, OpenCode-inspired
  JSONL trace adapter, Harn Cloud ingest-shaped JSON report, replay benchmark
  schema, and CI workflow template. The report includes replay-fidelity,
  permission-preservation, tool-call drift, transcript drift, observed-cost,
  and first-divergence triage metrics.
- **Generated provider catalog contract (#1584).** Added `harn providers`
  commands to refresh, validate, and export the provider/model catalog,
  checked-in JSON/JSON Schema artifacts, and TypeScript/Swift downstream
  bindings for aliases, variants, pricing, deprecation metadata, capabilities,
  and provider endpoint/auth metadata.

## v0.8.13

### Changed (breaking)

- **`daemon_spawn` config canonical field names.** Dropped the deprecated
  aliases `prompt` (use `task`), `state_dir` (use `persist_path`), and
  `queue_capacity` (use `event_queue_capacity`). The previous fallback
  chain that accepted either name has been removed; calls using the old
  names now error with `daemon_spawn: config must include \`task\`` (etc.).
  Migration: rename the keys in your `daemon_spawn({...})` calls. No
  in-repo callers used the deprecated aliases; this only affects external
  scripts.
- **Strict option-bag parsing for remaining stdlib agent/IO configs (#1568).**
  `spawn_agent`, `sub_agent_run`'s normalized host request, worker carry
  policy bags, worker snapshots, and `std/io.read_line` now reject unknown
  closed-schema option keys instead of silently ignoring typos.

### Added

- **Reusable ACP VM baselines (#1563).** File-backed `session/prompt` turns now
  cache a prepared VM baseline separately from the compiled bytecode cache, so
  repeated turns reuse stable stdlib/project/source setup while instantiating a
  clean execution VM for prompt globals, output, bridge state, tasks,
  cancellation, sync primitives, and shared state. ACP profile rollups now
  include a `vm_setup` bucket for prompt-turn setup diagnostics.
- **Profiling for ACP and benchmark workflows (#1559).** `harn serve acp` now
  supports `--profile`, `--profile-json`, and `--trace`, with
  `HARN_PROFILE`, `HARN_PROFILE_JSON`, and `HARN_TRACE` aliases. ACP profile
  JSON is appended as one NDJSON object per prompt turn. `harn bench` now shares
  the profile flags, reports p50/p95/stddev wall-time stats, and can write a
  JSON benchmark report with per-iteration profile rollups.
- **Typed stdlib option-bag + return-value records.** Started declaring
  structural `Ty::Shape` aliases for the stdlib's well-known dicts so
  literal-call sites get autocomplete and typo detection at parse time
  (matches the precedent set by `schema_recover`'s envelope). Applied to:
  `daemon_spawn` config + return (`DAEMON_CONFIG`, `DAEMON_SUMMARY`),
  `__io_read_line` (`READ_LINE_OPTIONS`, `IO_RESULT_ENVELOPE`),
  `__tui_page` (`PAGER_OPTIONS`), `__signal_on_interrupt`
  (`SIGNAL_HANDLER_OPTIONS`), `agent_session_seed_from_jsonl`
  (`AGENT_SESSION_SEED_OPTS`), `agent_session_compact`
  (`AGENT_SESSION_COMPACT_OPTS`), `agent_session_ancestry`
  (`SESSION_ANCESTRY` return), and `spawn_agent` (`WORKER_SUMMARY`
  return), `spawn_agent` config (`AGENT_SPAWN_CONFIG`), and
  `sub_agent_run` / `sub_agent_request` options (`SUB_AGENT_OPTIONS`).
  Literal worker/sub-agent option bags now reject unknown keys at `harn check`
  time, so misspelled worker/sub-agent options fail before runtime.
  `LLM_CALL_OPTIONS` remains defined and documented, with full
  enforcement deferred until internal callers (`agent_loop`, `agent_turn`)
  converge on the documented shape.

### Changed

- **Optional shape fields treat explicit `nil` as "missing".** Both the
  runtime type-check (`crates/harn-vm/src/typecheck.rs`) and the static
  type-checker (`crates/harn-parser/src/typechecker/inference/subtyping.rs`)
  now accept `{flag: nil}` against an optional shape field whose declared
  type doesn't include `nil`. Previously an explicit `nil` value had to
  match the declared field type; with this change, optional fields are
  treated uniformly as "the caller may omit the key OR pass `nil`",
  matching user intuition for option bags. Required fields still reject
  `nil` unless the declared type permits it.

### Internal

- **Shared option-bag parsing helpers** at
  `crates/harn-vm/src/stdlib/options.rs`. `OptionsParser` plus
  `dict_arg`/`required_string_arg`/`optional_string_arg`/`required_int_arg`
  let stdlib builtins extract option dicts without rebuilding
  `dict.get().map().filter().ok_or_else()` chains, with strict
  unknown-key detection available for closed-schema bags. `agents_daemon`
  and `agent_sessions` consolidated onto these helpers; the previous
  duplicate `require_dict_arg` / `required_string_arg` / `optional_string`
  / `opt_string` / `arg_string_required` / `arg_int_required` helpers
  with mismatched signatures and error kinds were removed.
- **Shared structural-record `Ty::Shape` aliases** at
  `crates/harn-parser/src/builtin_signatures/signatures/shapes.rs` for
  agent / sub-agent / daemon / LLM / IO / TUI / signal option bags and
  worker / daemon / session return contracts. See the "Added" entry above
  for which slots are wired up today.
- **Refactored `spawn_agent_builtin` and `sub_agent_run_builtin`** to share
  a single `finalize_and_run_worker` helper that handles
  persistence-snapshot, registry insertion, task spawn, optional terminal
  wait, and summary emission. Replaces ~14 duplicated lines of
  WorkerState lifecycle.

## v0.8.12

### Added

- **Cooperative process signal handlers (#1547).** Added `std/signal`
  with `on_interrupt`, `off_interrupt`, `interrupted`, and
  `with_interrupt`. `harn run` now routes SIGINT/SIGTERM/SIGHUP into VM
  interrupt dispatch with a graceful timeout, second-signal hard exit,
  and LIFO handler stacking, so long-running harnesses (chat loops,
  agent supervisors, polling jobs) can opt into clean shutdown without
  losing the panic-on-stuck escape hatch.
- **`std/dashboard/jobs` event envelopes (#1508).** Added
  `std/dashboard/jobs` with typed dashboard job event envelopes,
  validation, EventLog emission receipts, dedupe, and ordered Jobs view
  reduction. Local and cloud fixture streams cover runs, approvals,
  receipts, replay fixtures, and DLQ actions, and the host-facing
  dashboard contract is documented and wired through stdlib
  registration / conformance.
- **`std/tui::select_from` picker (#1544).** Added
  `select_from(items, opts?)` to `std/tui` so harness scripts stop
  hand-rolling fzf detection plus numbered-menu fallbacks. The picker
  auto-detects `fzf` then `gum choose` at runtime, falls back to a
  numbered `read_line` menu (which `mock_stdin` can drive in tests),
  and always returns a stable `{ok, value, status}` envelope. Supports
  `multi: true`, `default_index`, `cancel_value`, per-item `display`
  and `preview` callbacks, and
  `prefer_external: "auto" | "fzf" | "gum" | "none"` for forcing a
  backend.
- **JSONL-seeded agent sessions (#1546).** Added
  `agent_session_seed_from_jsonl(path, opts?)` to create a new
  first-class session from replayable `llm_transcript.jsonl` sidecars.
  The importer supports exact prompt-visible `message` events and older
  full request snapshots, with `truncate_to_last`, `drop_tool_calls`,
  `rename_session`, `validate`, `provider`, and `model` options. Agent
  loop sidecars now emit redacted `message` records as turns are
  appended, so future run artifacts can be lifted directly into a new
  prefix-cache-friendly session.
- **Structured `std/io` terminal input (#1542).** Added `std/io` with
  `is_tty(fd?)`, structured `read_line({prompt, timeout_ms, trim, echo, raw})`,
  `read_password`, and `write_stderr`. `read_line` returns explicit
  `ok`/`eof`/`timeout`/`interrupt`/`error` statuses, writes prompts to stderr
  with ANSI preserved, supports sub-second Unix timeouts without shelling out
  to `bash`, and disables terminal echo for password-style prompts.
- **Terminal pager helpers (#1543).** Added `std/tui` with `page(...)` for
  long text/markdown artifacts, `terminal_width()`, `rule()`, and `clear()`.
  `page` prints directly when stdout is not interactive, `no_pager` is set, or
  `$PAGER=cat`; otherwise it uses `$PAGER` with `less -R -F -X` defaults and
  falls back to print output when the pager binary is unavailable.
- **Interactive agent chat loop primitive (#1545).** Added `std/agent/chat`
  with `agent_chat_loop(...)`, `agent_chat_route_input(...)`, and
  `agent_chat_wait_for_user_tools(...)` so harnesses can share one
  operator-message / model-turn loop instead of reimplementing session
  management, slash-command routing, and `wait_for_user` turn stops. A
  post-turn callback can now set a typed `stop_reason` when it stops the
  loop, and `agent_session_close(id, status?)` records an
  `agent_session_closed` event before evicting the session so timeout and
  interruption closes are visible in event logs.

### Changed

- **`command_run` shell mode defaults to the host shell (#1549).**
  Shell-mode invocations of `command_run` (and downstream tools like
  `std/command::command_step`) no longer error when the caller omits
  both `shell` and `shell_id`. The host now resolves
  `discover_shells().default_shell_id` automatically (`bash` on macOS /
  Linux, `pwsh` on Windows). Malformed explicit `shell` / `shell_id`
  fields still error rather than silently falling back. Simplifies the
  common `mode: "shell"` call site to a single `command` field.
- **Wasmtime bumped to 44.0.1 (#1539).** Internal: harn-vm's optional
  Wasmtime/WASI stack moved from 29 to 44.0.1 with `wasmtime-wasi` on
  the current p1 feature. Sync WASIp1 modules now run on a dedicated
  host thread so testbench WASI subprocesses work from inside Harn's
  Tokio runtime while preserving mock clock and overlay state.

### Fixed

- **Escaped `${name}` in triple-quoted strings now passes through
  literally (#1548).** Multi-line / triple-quoted strings used to
  interpolate `${...}` even when escaped as `\${...}`, breaking any
  harness that wanted to emit literal shell variable references inside
  a multi-line script string. The lexer now treats `\${...}` as a
  literal `${...}` (matching single-line behavior) and preserves
  non-interpolation escapes like `\$PATH` unchanged.
- **Tail-call inside `for` loops leaked iterator state across the
  caller.** `return f(...)` from inside a `for x in xs { ... }` body
  is tail-call-optimized, but the new frame was capturing the current
  iterator depth instead of the popped frame's depth. The caller's
  outer iterator then iterated against the inner loop's leaked state,
  producing extra phantom items. The tail-called frame now inherits
  the popped frame's `saved_iterator_depth` and the iterator stack is
  truncated to match.

## v0.8.11

### Added

- **MCP Apps-compatible UI resource envelopes (#1505).** Added
  `std/ui_resource` for declaring `ui://` resources, tool-meta blocks, text
  and structured fallbacks, host capability negotiation, and the JSON-RPC
  `tools/call` / `context/update` message envelopes hosts proxy through
  `postMessage`. Resources validate through `std/artifact/web` (with
  `allow_host_bridge: true` by default since MCP Apps use `parent.postMessage`
  by contract) and ship with a CSP/sandbox dict that `ui_resource_csp_header`
  and `ui_resource_sandbox_attr` project into host headers/attrs.
  `ui_tool_result` always carries a non-empty text fallback so plain-text
  hosts still get useful output, and `ui_select_for_host` picks
  `ui_resource`, `structured_fallback`, or `text_fallback` based on host
  capability advertisements (MCP, OpenAI Apps SDK, or bare flags).
  `std/artifact/web` gained an `allow_host_bridge` option so the same safety
  rules apply to general artifacts while UI resources can keep their
  postMessage-based bridge. Examples ship under `examples/ui_resource/` for
  a dashboard widget and a multi-step review form.
- **Provider catalog refresh workflow and drift report (#1497).** Added
  `scripts/update_provider_catalog.harn`, a Harn-native workflow that
  collects model availability, pricing, and capability signals from
  provider sources, normalizes them with explicit provenance, and emits
  a markdown drift report + a TOML candidate patch under
  `.harn-runs/provider_catalog/`. The workflow never mutates shipped
  catalogs. New pure-logic library `scripts/provider_catalog_refresh.harn`
  handles observation merging (provider-owned beats aggregator-owned),
  drift detection against `llm_provider_catalog()`, and report
  rendering. Source adapters live in
  `scripts/provider_catalog_sources.harn` with three canonical shapes:
  `html_pricing_table_adapter` for keyless HTML pricing pages,
  `json_api_adapter` for public JSON APIs, and `key_required_adapter`
  that records `status: "skipped"` (instead of silently looking like a
  removal) when no API key is present. The bundled `--check` mode
  replays bundled HTTP fixtures and verifies the rendered report and
  candidate patch against committed goldens under
  `scripts/provider_catalog_fixtures/`. New
  `make check-provider-catalog-drift` runs the gate from `make all`.
  Coverage: 14 `@test` pipelines in
  `scripts/tests/provider_catalog_refresh_test.harn` cover the
  pure-logic helpers and every adapter shape (HTML, JSON, skipped
  key-required, key-required-with-key, conflicting observation,
  context-window-only change, fetch error).
- **Transparent profile bulletin schema and tool contract (#1506).** Added
  `std/personas/bulletins` with the `harn.profile_bulletin.v1` envelope for
  durable persona/user/project/team facts. Proposals carry stable id, scope,
  scope key, subject, persona, context key, assertion, status, confidence,
  structured evidence, source provenance, privacy/sync flags, and optional
  TTL/review timestamps. `bulletin_emit` always writes status `proposed` to
  `personas.bulletins.proposed`; hosts emit `harn.profile_bulletin_decision.v1`
  envelopes (`accept`, `reject`, `expire`, `supersede`) on
  `personas.bulletins.decisions` so review history is replayable.
  `bulletin_apply_decisions`, `bulletin_partition`, and `bulletin_active` keep
  prompt context derived from accepted, non-expired bulletins; the
  `bulletin_render_for_prompt` renderer visibly separates accepted facts from
  pending proposals so models cannot confuse them.
- **`std/edit` hardening for agent-authored mutations (#1499).** Made
  structural matching conservative by default: a structural fallback
  match now requires at least 3 non-blank needle lines and both the
  first and last anchor lines must carry a distinctive 4+ character
  alphanumeric token, so bare braces or short idents can no longer drive
  a wrong patch. The looser pre-existing behavior is opt-in via
  `structural_require_anchored_lines: "either" | "none"`,
  `structural_min_nonblank_lines: N`, and `structural_anchor_chars: N`.
  Expanded `lazy_placeholder` detection to cover `// ... rest`,
  `// TODO: implement|fill|add|complete`, `# ... rest`, `/* ... */`,
  `pass # ...`, and "unchanged" / "omitted for brevity" phrases. Added
  three new helpers — `edit_strip_line_number_prefixes` (strip leading
  `<spaces>N<space><pipe><space>` prefixes when ≥60% of non-empty lines
  carry them, with a matching `strip_line_numbers: true` option on
  `edit_apply_old_new_patch`), `edit_explain_whitespace_difference`
  (diagnose tabs-vs-spaces, base indent, or blank-line drift between a
  needle and the matched span), and `edit_check_lazy_truncation`
  (catch whole-file rewrites that shrank a file below `min_keep_pct`
  while still containing lazy placeholders). Successful line/structural
  matches surface a `whitespace_explanation` field, and `candidate_contexts`
  on ambiguous matches now uses a uniform `{start_line, end_line, snippet}`
  shape across exact, line, and structural modes. Also fixed a
  duplicate-candidate bug where structural matching would start from a
  leading blank line and report the same logical region twice. Locked in
  by an expanded `conformance/tests/stdlib/edit_patch.harn`.

### Fixed

- **Refresh `workflow-authoring-quickstart` pinned `graph_digest`.** The
  pinned bundle digest in `docs/src/workflow-authoring-quickstart.md`
  and `scripts/check_docs_workflow_quickstart.sh` had drifted from the
  current canonical encoding, breaking `make all`. Repinned to the
  current value so the doc's copy-paste path stays accurate.

## v0.8.10

### Added

- **`std/llm/economics` first-class cost helpers and explicit unknown
  pricing.** New stdlib module exposes `pricing_for`,
  `estimate_call_cost`, `estimate_session_cost`, `compare_model_costs`,
  `cache_break_even`, `volume_cost`, and `format_usd`, backed by new
  Rust builtins `llm_pricing`, `llm_compare_costs`, and `llm_format_usd`.
  The previous `model_pricing_per_million` table in
  `crates/harn-vm/src/llm/cost.rs` — annotated "as of early 2026" and
  prone to silent drift — has been removed; its canonical Anthropic /
  OpenAI / Gemini / Mistral entries are now exact-id catalog rows in
  `crates/harn-vm/src/llm_config.rs::canonical_priced_models()`,
  editable in one place under `git blame`. Pricing for unknown
  commercial models now surfaces as `pricing_known: false` /
  `cost_usd: nil` instead of silently coercing to $0; only providers
  explicitly configured with $0 rates (ollama, local, llamacpp, mlx,
  vllm, tgi) report cost=$0 with pricing_known=true. The CLI cost
  explainer (`harn run --explain-cost`) already routed through
  `llm_pricing_per_1k`, so its "unpriced" cell now reflects this
  stricter truth. New conformance tests under
  `conformance/tests/integration/llm_economics_*.harn` cover the
  helpers; existing `llm_cost`, budget, and routing behavior is
  unchanged for catalog-known models.
- **Safe HTML/CSS/JS artifact patching helpers (#1504).** Added `std/edit`
  for pure old/new text patches with exact, line-normalized, and structural
  matching, deterministic changed-region metadata, hashes, ambiguity
  diagnostics, and guardrails for no-op, whitespace-only, lazy-placeholder, and
  excessive-growth edits. Added `std/artifact/web` on top for small generated
  HTML artifacts: fragment extraction, validated patch application, text
  fallbacks, machine-readable reports for host approval UIs, and checks for
  obvious network calls, external resources, host bridge calls, dangerous
  navigation, inline secrets, and broken core tags.

## v0.8.9

### Fixed

- **Workflow stages now thread `iteration_budget` through to per-stage
  `agent_loop`.** `ModelPolicy` carried a `max_iterations` field but
  not `iteration_budget`, so any workflow node policy authored through
  `agent_preset(...)` / `agent_budget(...)` silently lost its adaptive
  budget at the serde boundary in `workflow_commit`. The per-stage
  `agent_loop` then fell back to the static `max_iterations: 16`
  default and never emitted `loop_control_decision` events, defeating
  the v0.7.61 adaptive-budget contract for any harness that authored
  per-stage policies through the std presets. The fix adds a typed
  `iteration_budget: Option<serde_json::Value>` field to `ModelPolicy`
  in `crates/harn-vm/src/orchestration/policy/types.rs`, and threads
  the same key through the `loop_option_keys` list in
  `std/workflow/options.harn` so `workflow_stage_agent_options` passes
  it to the per-stage `agent_loop`. `agent_loop`'s
  `__normalize_iteration_budget` already prefers `iteration_budget`
  over `max_iterations` when both are present, so callers that supply
  both still get the adaptive shape and `loop_control_decision`
  visibility. Locked in by
  `conformance/tests/agents/workflow_stage_options_iteration_budget.harn`.

## v0.8.8

### Added

- **Persona supervision-runtime hooks (#1480).** `harn_vm::personas`
  now exposes a `PersonaSupervisionSink` mirroring the harn-cloud
  multiplexed `persona/update` feed for the runtime-sourced
  `update_kind`s. The runtime emits typed `queue_position` events on
  every enqueue/drain transition, `receipt` events at the tail of
  every `run_for_envelope` call, and `repair_worker_status` events
  through a new `report_repair_worker_status` entry point that is
  append-only and idempotent on `(repair_worker_id, lifecycle)`. A
  typed `restore_persona_checkpoint` entry point acks supervision-API
  restore requests by emitting a `Checkpoint(action: RestoreAcked)`
  envelope carrying the resume coordinates the runtime actually
  resumed from (`run_id`, `lease_id`, `last_run_ms`,
  `queued_work_keys`). All emissions accept `now_ms` from the caller,
  so wiring through `harn_clock::RecordedClock` produces
  byte-identical sink sequences across replays — locked in by a new
  determinism test in `crates/harn-vm/src/personas.rs`.

## v0.8.7

### Fixed

- **Preserve `->` arrow on zero-arg closure literals in `harn fmt`.**
  The formatter was silently rewriting `{ -> expr }` into `{ expr }`,
  which changes meaning from "closure that returns expr" to "block
  expression evaluated immediately." Any call site like `f()` then
  failed at runtime because `f` was no longer callable. The fix emits
  `->` for closure literals regardless of whether the parameter list
  is empty, in both the inline and multi-line code paths. Locked in
  by new round-trip tests in `crates/harn-fmt` and the conformance
  fixture
  `conformance/tests/fmt/zero_arg_closure_arrow_preserved.harn`.

### Added

- **`std/cache` levels up to a cost-moat substrate (#1473).** The
  module now ships three composable backends —
  `mem_cache(opts?)` (thread-local LRU; per-VM, does not survive
  `harn run`), `fs_cache(path, opts?)` (content-addressed JSON files
  with atomic writes), and `sqlite_cache(path, opts?)` (one sqlite
  file, many namespaces, TTL + LRU eviction inside the put
  transaction). All three accept the same `namespace`/`ttl`/
  `ttl_seconds`/`max_entries` options and surface a uniform
  `{hit, value?, backend, namespace}` envelope. A generic
  `with_cache(key, compute, options?)` helper wraps any 0-arity
  closure (with `with_cache_envelope` for inline hit/miss/metrics),
  and `cache_stats(options?)` / `cache_stats_reset(options?)` expose
  in-process hit/miss counters per namespace. Cache TTL now reads
  from the unified clock (`mock_time` / `advance_time` honored), so
  testbench fixtures can reproduce expiry windows without wall-clock
  flakiness. When `options.session_id` is set, `with_cache` emits
  `cache_hit` / `cache_miss` events on the agent event tape (now
  registered as first-class `AgentEvent` variants so ACP and closure
  subscribers see them) with cost-moat receipts
  (`model_calls_avoided`, `tokens_saved` from `usage.input_tokens` +
  `usage.output_tokens`, `latency_saved_ms` from `latency_ms`) for the
  persona value ledger (harn-cloud#58) and crystallization receipts.
  Both the `with_cache(next, opts?)` middleware form and the direct
  `with_cache(prompt, system, opts)` form in `std/llm/handlers` cache
  and emit receipts; they share one cache key. Docs at
  [`docs/src/stdlib/cache.md`](docs/src/stdlib/cache.md).
- **`std/agent/presets` captain preset pack and active
  `require_successful_tools` gate.** Closes #1472. The four captain
  presets — `merge_captain`, `review_captain`, `oncall_captain`,
  `release_captain` — package the persona-shaped service contracts
  adopters were re-deriving by hand: a long-enough adaptive iteration
  budget, the cheap-default + frontier-escalation `with_routing`
  scaffolding, and the per-captain governance defaults (`merge_captain`
  ships a default `with_consent` that auto-approves tools annotated
  `read`/`search`/`fetch`/`think` and denies everything else;
  `oncall_captain` ships a default `with_rate_limit({max_calls: 50})`
  cap; `release_captain` accepts an opt-in `with_dry_run` shadow-run
  layer). Each captain composes the
  [`std/llm/handlers`](src/stdlib/llm-handlers.md) handler stack and
  the [`std/llm/tool_middleware`](src/stdlib/tool-middleware.md)
  middleware stack from caller-supplied `audit_sink` /
  `telemetry_sink` / `consent` / `rate_limit` / `handoff_sink` /
  `cheap_caller` / `frontier_caller` / `escalate_predicate` /
  `logging_sink` options, so persona manifests (#460) reference
  contracts by name instead of duplicating wiring. Each captain ships
  a corresponding `*_captain_agent(prompt, options?)` wrapper that
  spreads the preset directly into `agent_loop`.
  `require_successful_tools` is now an active gate that returns a
  structured `error_envelope` (consumable by harn-cloud receipts) and
  emits a `tool_gap` friction event onto the friction sink so
  context-pack suggestions and dashboards pick it up alongside
  natively-emitted events. The agent_loop tool envelope now carries
  `annotations` (verbatim from the tool entry) so `with_consent` and
  other middleware can policy-gate by tool kind without crawling
  `schema.annotations`. Five new conformance tests under
  `conformance/tests/agents/`: `agent_preset_captains`,
  `agent_preset_captain_cheap_route`,
  `agent_loop_require_successful_tools_friction`, and the existing
  `agent_loop_adaptive_budget` plus a new
  `agent_loop_adaptive_budget_mock_time` (proves the budget extension
  decisions don't quietly observe wall-clock time). Documented in
  `docs/src/llm/agent_loop.md`. (#1472)

## v0.8.6

### Added

- **Annotation tape format for `harn test-bench`.** A new sidecar
  format (`<tape>.annotations.jsonl`) attaches structured human
  judgment — `correct`, `incorrect`, `alternative`, `note`, `marker`,
  `mute`, `hypothesis`, `friction`, `crystallize_here` — to specific
  events on a recorded testbench tape. Annotations are versioned JSONL
  with a header carrying an optional `tape_content_hash` so the
  validator catches tape edits that invalidate `event_id` references.
  `friction` annotations adapt directly to `FrictionEvent` records so
  they feed `orchestration::generate_context_pack_suggestions`
  alongside natively-emitted events; `crystallize_here` annotations
  surface as `CrystallizeAnchor` records ready for the
  candidate-detection pipeline. Three new CLI surfaces:
  `harn test-bench replay --annotations <path>` (validates + surfaces
  annotations inline during replay), `harn test-bench
  validate-annotations` (structured JSON report, exits `2` on any
  problem), and `harn test-bench export-annotations --kind ... --format
  jsonl|friction` (filter + re-emit for downstream pipelines). The
  conformance runner picks up `<name>.annotations.jsonl` sidecars
  automatically and gates the test on validation success; covered by
  `conformance/tests/testbench/testbench_replay_fidelity.annotations.jsonl`
  plus four `harn-cli` integration tests under
  `tests/test_bench_cli.rs::annotations`. Documented in
  `docs/src/dev/annotation-tape-format.md`. (#1474)
- **`std/llm/handlers` cost-moat handlers — `with_repair`,
  `with_coerce`, `with_timeout`, `with_routing`.** Closes the persona
  platform's "cost moat substrate" gap from #1470: the four handlers
  let any caller compose cheap-model-by-default with frontier
  escalation (`with_routing`), per-call deadlines that honor the
  unified clock and forward `timeout_ms` to providers
  (`with_timeout`), one-shot schema-validation repair with a
  deterministic corrective nudge (`with_repair`), and uniform
  case-insensitive key normalization on the success envelope
  (`with_coerce`). `safe_structured_call`'s structured-output dance is
  now documented as the canonical preset equivalent to
  `compose([with_coerce({})])(structured_caller)`, with judge-friendly
  defaults baked in. Each handler ships with a conformance gate under
  `conformance/tests/integration/llm_handlers_with_*` that exercises
  the success and edge-case paths without reaching for a live
  provider; the persona-shaped composition is documented in
  `docs/src/stdlib/llm-handlers.md` and the quickref. (#1470)
- **Partial-application form for `(next, opts)` handler middleware.**
  `with_retry`, `with_logging`, `with_budget`, `with_circuit_breaker`,
  `with_repair`, `with_coerce`, and `with_timeout` now accept either
  `with_X(next, opts)` (direct) or `with_X(opts)` (curried — returns a
  wrapper for `compose`). The auto-currying makes the canonical
  `compose([with_logging({...}), with_retry({...})])(base)` pattern
  documented in the quickref and llm-handlers reference actually work
  — previously the opts dict was silently bound as `next` and the
  composition failed at first invocation. A new
  `llm_handlers_persona_compose` conformance fixture pins the
  end-to-end persona-shaped chain (routing + budget + logging) so the
  cost-moat substrate stays exercised. (#1470)
- **Testbench clock-leak audit.** New `crate::clock_mock::leak_audit`
  shim records every `capability_id` that observes the OS wall or
  monotonic clock while a testbench mock is installed.
  `TestbenchSession::finalize` now returns a `clock_leaks` vector
  alongside `fs_diff` / `recorded_subprocesses` / `tape`; `harn
  test-bench run` prints `[testbench] clock leak: <capability>
  (count=N)` to stderr for each unique entry; scripts introspect via
  the new `testbench_clock_leaks()` builtin. Three demonstration call
  sites (`stdlib/date_iso`, `host_call/process.exec.{started_at,
  ended_at}`) are migrated to the audited helpers, and the new
  `conformance/tests/testbench/testbench_clock_leak_warns.harn` case
  pins the contract. Closes the testbench-mode epic (#1438) and its
  fidelity-audit follow-up (#1466).
- **String escape sequences `\r` and `\0`.** Double-quoted string
  literals now recognize `\r` (carriage return) and `\0` (NUL) in
  addition to the existing `\n`, `\t`, `\\`, `\"`, `\$`. Triple-quoted
  multiline strings remain literal. The formatter emits `\r` / `\0`
  back out for those bytes when re-rendering string literals so
  round-trips stay readable. Spec updated in
  `spec/HARN_SPEC.md#single-line-strings`.
- **Repo automation scripts ported to Harn.** Five one-off
  Python/Bash scripts (`check_xfail_count`, `compare_generated_text`,
  `sync_language_spec`, `detect_bump_type`, `check_openapi_snapshot`)
  are now `.harn` files under `scripts/`, with companion
  `@test`-pipeline coverage in `scripts/tests/`. Wired into a new
  `make test-harn-scripts` target and into `make all`; the existing
  pre-commit `harn fmt` / `harn lint` stanza now covers `scripts/`
  too, so these scripts stay typecheck-clean and warning-free as
  they evolve.
- **`harn test-bench run --runtime des` (single-threaded testbench).**
  Opt-in flag that swaps the testbench's default multi-thread Tokio
  runtime for a `current_thread` runtime so all VM tasks, I/O
  callbacks, and timer firings share one OS thread. Eliminates
  inter-thread scheduling races and yields bit-exact event tapes for
  scripts that stay within the DES-safe primitive set. Adds three
  `testbench_*` conformance fixtures under
  `conformance/tests/testbench/` plus three `des_runtime_*` Rust
  integration tests covering paused-sleep, byte-identical concurrent
  settle, and parity with `--runtime paused-tokio`. The constraint
  surface, benchmark methodology, and the decision to ship as opt-in
  (rather than the default) are written up in
  `docs/src/dev/des-mode.md`. (#1444)
- **`harn fmt` normalizes trailing commas, and the `trailing-comma`
  rule is now AST-aware.** Previously the rule walked tokens with a
  brace-vs-block heuristic that fired stray "missing trailing comma"
  diagnostics on `eval_pack { cases: ... summarize { ... } }` and
  multi-field `struct Foo { bar: int fn name() ... }`, sometimes
  inserting commas that broke the source. Trailing-comma detection
  now runs against the parsed AST so it only fires on confirmed
  comma-separated bracket pairs (call args, list/dict/struct
  literals, selective imports), and `harn fmt` applies the same
  normalization as a post-pass — `xs.filter(...).to_list()`-style
  multi-line constructs round-trip cleanly without relying on `harn
  lint --fix`. The traversal lives in `harn_parser::visit` and is
  reused by the linter.
- **Iter-style sinks (`.to_list()`, `.to_set()`, `.to_dict()`) work
  on already-realized collections.** `xs.filter(...).to_list()` used
  to silently return `nil` because list method dispatch had no
  handler for `to_list` and fell through a silent-`Nil` catch-all.
  List/dict/set now expose `to_list` / `to_set` / `to_dict` as the
  obvious identity/conversion (dict→list yields key/value entry
  dicts to match `.entries()`), so the same chain works whether
  `filter` returned an `iter` or a `list`.
- **Method dispatch raises a clear error on unknown methods instead
  of returning `nil`.** Every per-type method handler (list, dict,
  set, string, generator, stream, iter, plus the outer dispatch)
  used to bottom out in `Ok(VmValue::Nil)`, which made typos in
  method names fail silently far from the call site. Unknown method
  calls now throw a runtime error of the form ``list has no method
  `whatever``` so issues surface at the offending line. *Breaking*
  for any script that relied on the silent-`nil` fallthrough.
- **`cargo build` self-heals git hook configuration.** The
  `harn-cli` build script now resets `core.hooksPath` to `.githooks`
  whenever it drifts (the most common cause of CI surprises being
  pre-commit / pre-push hooks that *would* have caught a `harn fmt
  --check` regression but never ran because the user's hook config
  pointed elsewhere). Set `HARN_DISABLE_AUTO_HOOK_SETUP=1` to opt
  out. Skipped automatically when not building inside the Harn
  source tree.
- **`websocket_connect` retries transient TCP errors during
  handshake.** Connection-reset / broken-pipe / unexpected-EOF on
  the very first attempt — typically caused by the OS recycling an
  ephemeral port still in TIME_WAIT — now self-heal with up to two
  retries within the user's `timeout_ms` budget. Permanent failures
  (DNS, refused, TLS, protocol) bypass the retry path and surface
  immediately.
- **Package manager maturity: provenance, outdated, audit, and
  generated-artifact contract checks.** Bumped `harn.lock` to version 2
  with top-level `generator_version` / `protocol_artifact_version` and
  per-entry `package_version`, `harn_compat`, `manifest_digest`, and
  `[package.registry]` provenance for entries originally added through
  the registry index (the manifest now also stores `registry`,
  `registry_name`, `registry_version` so the source survives round-trips).
  Three new subcommands give downstream automation a single Harn binary
  to call: `harn package outdated` (with `--remote` for branch-tracking
  git deps), `harn package audit` (stable JSON `code` per finding for
  yanked-version, content-hash, manifest-digest, and harn-range
  violations), and `harn package artifacts manifest|check` for vendoring
  and drift-checking the protocol-artifact manifest. `harn install` and
  `harn update` now accept `--json`. v1 lockfiles migrate transparently
  on the next install. Documented the cross-repo bump workflow in
  `docs/src/package-authoring.md`. (#1428)
- **Workflow patch proposals + safe Harn function tools.** Agents can now
  author bounded, auditable changes to a workflow bundle through a flat
  patch JSON (`insert_node`, `add_edge`, `upsert_prompt_capsule`,
  `update_node_policy`, `update_bundle_policy`) instead of regenerating
  the whole bundle. Three new `harn workflow patch
  {validate,apply,preview}` subcommands apply the patch to a copy,
  re-run the bundle validator, emit a structural diff, and reject
  anything that widens the parent capability ceiling along *tools*,
  *capabilities*, *side-effect level*, *workspace roots*, *connector
  scopes*, *command gates*, or *autonomy tier*. `harn workflow
  function-tools` enumerates an allowlist of read-only / pure-think
  Harn functions an agent may call from inside the patch loop, each
  carrying an ACP-aligned `ToolAnnotations` block hosts can wire
  straight into a model surface. `harn workflow nested-ceiling`
  exposes the same scanner used internally so hosts can reject nested
  Harn invocations (workflow bundles, Harn scripts, Burin harness
  manifests) that would widen the active execution policy. The
  workflow-authoring skill pack and `docs/src/workflow-bundles.md`
  cover the contract; a new
  `crates/harn-cli/tests/workflow_patch_cli.rs` gate exercises the
  validate / apply / preview / function-tools / nested-ceiling surfaces
  end-to-end. (#1423)
- **Python and Go protocol bindings.** Extended
  `harn dump-protocol-artifacts` to emit a stdlib-only Python 3.9+ module
  (`spec/protocol-artifacts/python/harn_protocol.py`) and a Go package
  (`spec/protocol-artifacts/go/harnprotocol/`) mirroring the existing
  TypeScript and Swift surface: ACP session updates, JSON-RPC envelopes,
  Harn tool lifecycle metadata, A2A task structures, and MCP tool/resource
  records. `manifest.json` now exposes a `bindings` block with per-language
  stability and module-path metadata so downstream consumers (Burin Code,
  Harn Cloud, Python integrators, Go workers) can detect generator/runtime
  mismatch without bespoke compatibility checks. A new `make check-bindings`
  target round-trips a checked-in JSON fixture
  (`spec/protocol-artifacts/fixtures/round_trip.json`) through both bindings
  and runs in CI to catch wire-vocabulary drift before downstream consumers
  see it. (#1429)
- **Workflow-authoring skill pack and small-model evals.** Added
  `examples/skill-packs/workflow-authoring/` with a top-level `SKILL.md`,
  small-model prompting guide, validated PR-monitor and PR-repair recipe
  bundles, eval cases with structural assertions, and an `eval.harn` driver
  that feeds a configurable provider through the validate → preview → run
  pipeline. A new `crates/harn-cli/tests/workflow_authoring_eval.rs`
  regression gate fails CI when a recipe golden or a case's structural
  assertions drift.
- **Workflow authoring quickstart.** New tutorial at
  `docs/src/workflow-authoring-quickstart.md` walks `validate` →
  `preview` → `run` → connector status/setup-plan → supervisor in one
  copy-paste path with no paid credentials. Backed by checked-in
  fixtures (`docs/fixtures/workflow-bundles/quickstart-{minimal,agentic}.bundle.json`,
  `docs/fixtures/connect-demo/`) and a CI gate
  (`make check-docs-workflow-quickstart`) that pins the deterministic
  bundle digest, executed-node sequence, and connector-status shape so
  the snippets cannot drift.
- **Cross-platform release smoke matrix.** Added
  `.github/workflows/release-smoke.yml`, `scripts/release_smoke.sh`,
  and `tests/smoke/` fixtures so every release-relevant PR runs the
  user-visible CLI surface (`--help`, `check`, `fmt --check`,
  `package check`, `--provider-matrix`, `run`, `command_run`,
  no-credentials mock workflow, `harn watch` boot) on macOS, Linux,
  and Windows. Failures surface as
  `::error::release-smoke (<platform>): <capability> failed`
  annotations and the smoke audit lane is now part of
  `release_gate.sh audit`.
- **Platform compatibility docs.** Added
  [docs/src/dev/platform-compatibility.md](docs/src/dev/platform-compatibility.md)
  with a per-capability support matrix and rationale for the
  Windows-deferred features (POSIX-signal drain, `unveil`/`pledge`).
- **Tag-first publish trigger.** `publish-release.yml` now also fires
  on `push: tags: ['v*']`, in addition to the existing `push: main`
  drift trigger. Detect-drift recognizes the tag-push event and sets
  `publish_ref=$tag, drift=true` so the publish job checks out the
  tagged commit (detached) and ships from there. Lets the bump-fleet
  `release_harn.harn` harness push `vX.Y.Z` at a pinned commit BEFORE
  the Release PR merges — what gets shipped to crates.io and the
  GitHub release is anchored to that exact commit, and commits that
  land on `main` between PR-open and merge cannot leak into the
  published artifact. The `push: main` drift trigger remains as the
  legacy/recovery path for releases authored without the harness;
  workflow_dispatch with no drift still recovers from an existing
  tag.

### Fixed

- **Recognize `/** */` doc comments in `harn package check`.** The
  publish-readiness check previously saw only `///` doc comments, so
  the canonical HarnDoc form preferred by the linter would surface
  spurious "no doc comment" warnings on otherwise-documented public
  symbols. Both forms now produce identical `docs` bodies.
- **Typed stdlib option/result shapes.** `std/collections.filter_nil` and
  `pick_keys`, plus `std/json.merge`/`pick`/`omit`, are now generic over the
  value type — a `dict<string, V>` (or homogeneous shape literal) projects back
  to a dict that still carries `V`. Introduced `PickKeysOptions` and the
  workflow `WorkflowAutonomyPolicyConfig`/`WorkflowModelPolicy`/
  `WorkflowStageOptionsConfig`/`WorkflowStageAgentOptions` and connector
  `GitHubConnectorConfig`/`GitHubCallOptions`/`GitHubWaitOptions` shapes so
  the high-traffic agent/workflow and connector paths advertise their
  contract instead of accepting freeform `dict`.
- **Testbench conformance suite.** Added `conformance/tests/testbench/`
  covering the deterministic axes of `harn test-bench`: paused-clock
  sleep, 30 simulated cron days under one `trigger_test_harness` call,
  100 concurrent mocked agents settling deterministically, recorded
  subprocess replay, copy-on-write fs overlay diff, deny-by-default
  network egress, and byte-identical replay fidelity against a checked-in
  event tape. The conformance runner now activates the testbench session
  automatically when sidecar files (`.process-tape.json`, `.fs-overlay/`,
  `.testbench-tape`) are present next to a `.harn` test, and two new
  script-side builtins (`testbench_is_active`, `testbench_fs_diff`)
  expose the active overlay diff to assertions. The eighth case from
  the original issue (a runtime audit warning when a host capability
  bypasses the unified mock clock) is filed against #1466 since it
  depends on a runtime feature that doesn't exist yet. (#1442)

### Changed

- **Type-checker generics.** `dict<string, V>` parameter slots now bind `V`
  from a heterogeneous shape literal (union of field types) so generic
  stdlib helpers preserve element typing through projection.
  Optional shape fields validate the value type when supplied — a
  `{drop_nil?: bool}` parameter rejects `{drop_nil: "yes"}` instead of
  silently accepting it.
- **Cross-module type aliases.** Selectively importing a function (e.g.
  `import { pick_keys } from "std/collections"`) now also pulls every
  exported type alias / struct / enum / interface from the same module
  into scope so call-site contract checks resolve referenced shapes
  instead of seeing phantom `Named("PickKeysOptions")`.
- **Return-type checking scope.** `fn` return-type validation now runs
  against the post-body scope (with narrowing rolled back) so values bound
  by `let`/`var` inside the body resolve correctly when reused in a
  structural return literal.

## v0.8.5

### Added

- **Python and Go protocol bindings.** Extended `harn dump-protocol-artifacts`
  to emit Python and Go bindings mirroring the existing TypeScript and Swift
  surface. The `manifest.json` now exposes a `bindings` block with per-language
  stability and module-path metadata so downstream consumers can detect
  generator/runtime mismatch without bespoke checks. A new `make check-bindings`
  target round-trips a JSON fixture through both bindings to catch wire-vocabulary
  drift. (#1429)
- **Workflow authoring skill pack and evals.** Added `examples/skill-packs/`
  with a top-level `SKILL.md`, small-model prompting guide, PR-monitor and
  PR-repair recipe bundles, and eval cases with structural assertions. A new
  `crates/harn-cli/tests/workflow_authoring_eval.rs` regression gate fails CI
  when a recipe golden or a case's structural assertions drift. (#1435)
- **Workflow authoring quickstart.** New tutorial at `docs/src/workflow-authoring-quickstart.md`
  walks `validate` → `preview` → `run` → connector status/setup-plan → supervisor
  in one copy-paste path with no paid credentials. Backed by checked-in fixtures
  and a CI gate that pins the deterministic bundle digest, executed-node sequence,
  and connector-status shape. (#1436)
- **Cross-platform release smoke matrix.** Added `.github/workflows/release-smoke.yml`,
  `scripts/release_smoke.sh`, and `tests/smoke/` fixtures so every release-relevant
  PR runs the user-visible CLI surface on macOS, Linux, and Windows. Failures
  surface as `::error::release-smoke` annotations in the smoke audit lane. (#1434)
- **Platform compatibility docs.** Added `docs/src/dev/platform-compatibility.md`
  with a per-capability support matrix and rationale for Windows-deferred
  features (POSIX-signal drain, `unveil`/`pledge`). (#1434)
- **Testbench composition primitive.** Wires Harn's deterministic substrate —
  virtual time, mocked LLM, filesystem overlay, recorded subprocess, and a
  deny-by-default network — behind a single `Testbench` handle and a new
  `harn test-bench` CLI surface. Production wires the real implementations;
  tests/demos pick a config and get an audit trail of every host boundary
  crossing. (#1440)
- **Unified redaction policy.** Added a unified redaction policy across
  persistence surfaces. Includes `crates/harn-vm/src/redact/` and `patterns.rs`
  for consistent masking in transcripts, receipts, and event logs. (#1445)
- **Unified clock crate.** Extracted a `harn-clock` crate providing the `Clock`
  trait with `RealClock`, `PausedClock`, and `RecordedClock`. Unifies
  deterministic clocks across VM, stdlib, conformance, and CLI. (#1446, #1449,
  #1450)
- **Tag-first publish trigger.** `publish-release.yml` now fires on `push: tags:
  ['v*']` in addition to `push: main`. Detect-drift recognizes the tag-push
  event and sets `publish_ref=$tag, drift=true` so the publish job checks out
  the tagged commit (detached) and ships from there. This anchors what reaches
  crates.io and the GitHub release to the pinned commit, preventing commits
  landing on `main` between PR-open and merge from leaking. (#1456)

### Changed

- **Type-checker generics.** `dict<string, V>` parameter slots now bind `V`
  from a heterogeneous shape literal (union of field types) so generic stdlib
  helpers preserve element typing through projection. Optional shape fields
  validate the value type when supplied — a `{drop_nil?: bool}` parameter
  rejects `{drop_nil: "yes"}` instead of silently accepting it. (#1448)
- **Cross-module type aliases.** Selectively importing a function now also pulls
  every exported type alias / struct / enum / interface from the same module
  into scope so call-site contract checks resolve referenced shapes. (#1448)
- **Return-type checking scope.** `fn` return-type validation now runs against
  the post-body scope (with narrowing rolled back) so values bound by `let`/`var`
  inside the body resolve correctly when reused in a structural return literal.
  (#1448)
- **Typed stdlib option/result shapes.** `std/collections.filter_nil` and
  `pick_keys`, plus `std/json.merge`/`pick`/`omit`, are now generic over the
  value type — a `dict<string, V>` projects back to a dict that still carries
  `V`. Introduced `PickKeysOptions` and workflow/connector shape types so high-
  traffic paths advertise their contract instead of accepting freeform `dict`.
  (#1448)

### Fixed

- **Recognize `/** */` doc comments in `harn package check`.** The publish-
  readiness check previously saw only `///` doc comments, so the canonical
  HarnDoc form preferred by the linter would surface spurious "no doc comment"
  warnings on otherwise-documented public symbols. Both forms now produce
  identical `docs` bodies. (#1448)
- **Improve diagnostics for shape, property, and stdlib contract failures.**
  Common authoring failures around structured values now surface with actionable,
  span-anchored messages instead of bottoming out as generic nil/property
  runtime errors. Property access on `nil` includes `?.` / nil-guard
  suggestions; `__assert_shape` and `schema_recover` validator append actually-
  present keys to missing-field errors. (#1455)
- **Expand `harn doctor`.** Expanded `harn doctor` for one-command environment
  readiness checks, improving the initial setup and troubleshooting experience.
  (#1437)
- **Add Bitbucket entry to connector parity matrix.** Added Bitbucket entry to
  the connector parity matrix, ensuring parity tracking is up to date. (#1451)

## v0.8.4

### Added

- Preserve structured initial user content in agent sessions so ACP multimodal
  prompt blocks reach the agent transcript.
- Mark Ollama Gemma 4 routes as vision-capable in the Harn capability matrix.
- Export normalized workflow bundle graphs as JSON, including graph nodes/edges,
  node-scoped diagnostics, editable field metadata, and embedded Mermaid
  diagrams.
- Add `harn workflow preview --mermaid` for a debug diagram view of workflow bundles.
- Generate complete Swift protocol contracts from Harn, including ACP
  method/notification enums, MCP/A2A constants, JSON-RPC IDs, JSON values,
  response/error helpers, and executor display labels.

### Changed

- Standardize workflow message handling in the stdlib to support the new
  multimodal session input flow.
- Align generated protocol README with the artifact ownership boundary for
  downstream hosts.

## v0.8.3

### Added

- **Portable workflow bundle CLI.** Added the `harn workflow validate`,
  `harn workflow preview`, and `harn workflow run` commands for JSON workflow
  bundle validation, graph preview, and deterministic local run receipts,
  alongside the v1 bundle contract docs and a GitHub PR monitor fixture.
- **Agent runtime reliability profiles.** Added runtime context-window metadata
  to provider/model catalogs, exposed it through `model_info` and CLI catalog
  dumps, and taught Ollama calls plus stdlib token budgeting to prefer runtime
  context windows when local serving capacity differs from model maximums.

### Changed

- **Agent option forwarding.** Consolidated stdlib agent option forwarding with
  `pick_keys`, so worker and simulated-user helpers carry reasoning policy,
  scale, task, and runtime options without repetitive per-field plumbing.
- **Manual skill routing.** Added a manual skill-matching strategy for callers
  that already select active skills, avoiding redundant catalog prompt/scoring
  while still loading explicitly selected skills.
- **Provider capability profiles.** Added Qwen3-Coder-specific provider rules
  across OpenRouter, Hugging Face, and Together so agent defaults avoid known
  broken thinking-mode behavior.

### Fixed

- **Reasoning disable semantics.** Made explicit `reasoning_effort: "none"`
  disable thinking even when the provider does not support effort levels, and
  reported disabled thinking consistently in agent observation metadata.
- **Agent loop resilience.** Let completion judges fail open on provider errors
  when configured, made transcript auto-compaction honor token thresholds, and
  applied command-policy gating to host-bridged `process.exec` calls.

## v0.8.2

### Added

- **Simulated users for eval harnesses.** Added `std/agent/user` with
  `agentic_user`, deterministic `scripted_user` / `fixture_user`, an `ask_user`
  tool adapter, post-turn callback adapter, reply/LLM-call guardrails, and
  audit events for harnesses that need a model or fixture to substitute for the
  user during agent evals.

### Changed

- Add provider-aware agent reasoning policy (#1406).
- Clean stale Harn dev target dirs (#1405).
- Add simulated user helpers (#1404).
- Harden agent completion and skill routing (#1403).
- Capture agent_loop provider failures (#1399).
- Add streaming-native trigger primitives (#1401).
- Harden persona runtime status (#1400).
- Add agent loop stall diagnostics (#1398).
- Make MCP server context first-class (#1395).
- Add compile-time capability policy invariants (#1393).
- Add workflow crystallization v2 shadow receipts (#1394).
- Add review and on-call persona packages (#1392).
- Reduce VM hot-path allocations (#1391).

## v0.8.1

### Added

- **Orchestrator replay determinism oracle.** A new conformance suite
  under `conformance/replay-oracle/` validates that orchestrator execution is
  deterministic across runs, covering tool calls, DLQ retries, LLM predicates,
  local triggers, and A2A worker paths.
- **Session system prompt composition.** Agents now support composing system
  prompts from multiple parts, with updated stdlib signatures in
  `harn-stdlib` and new helpers in the VM for session configuration and
  transcript management.
- **Compiler optimization pipeline.** The VM now includes an optimization pass
  (e.g., constant folding) with a new `crates/harn-vm/src/compiler/optimizer.rs`
  module and corresponding conformance tests.
- **Protocol conformance matrix.** Expanded fixture coverage for A2A, ACP,
  and MCP protocols, including agent card adapters, security constraints,
  session updates, JSON-RPC error handling, and tool/resource boundaries,
  alongside updated JSON schemas.
- **Generated protocol artifacts for downstream bindings.** Added
  `harn dump-protocol-artifacts`, Make targets for generation and drift checks,
  and checked-in ACP/A2A/MCP schema copies plus TypeScript and Swift bindings
  under `spec/protocol-artifacts/`.
- **Split protocol adapters into focused modules.** A2A, ACP, and MCP adapters
  in `harn-serve` are reorganized into dedicated submodules (auth, events,
  schema, tasks, sessions, transport) with dedicated test suites, improving
  maintainability without changing external behavior.

### Fixed

- **Package and listener test hermeticity.** Made package and listener tests
  hermetic by tightening test support fixtures and lockfile/manifest/registry
  handling in `harn-cli`.
- **Tree-sitter parsing for A2A regex fixtures.** Fixed parsing issues in
  A2A-related conformance test fixtures.
- **Ollama tool argument replay.** Corrected replay behavior for Ollama tool
  arguments in the LLM tool messages module.

### Changed

- **Removed manifest provider static leaks.** Cleaned up static state leaks in
  manifest providers and trigger event handling, improving memory safety in
  `harn-vm` and `harn-cli` package extensions.

## v0.8.0

### Added

- **Composable tool middleware (`std/llm/tool_middleware`).** New parallel
  seam to `llm_caller`: `agent_loop` now accepts a `tool_caller:` closure that
  wraps every tool dispatch (across all executors — `harn`, `host_bridge`,
  `mcp_server`, `provider_native`). Paired with a schema-time decorator
  (`tools_use_middleware(registry, transform)` + `tool_inject_param`) so
  middleware can also augment the tool schemas the model sees. Ships
  `compose_tool_callers([..])`, `default_tool_caller()`, and a bundled
  middleware library: `with_required_reason` (force a `reason` arg on every
  tool call and surface it as a user-facing summary), `with_audit_log`,
  `with_consent`, `with_dry_run`, `with_redaction`, `with_idempotency`,
  `with_rate_limit`, `with_telemetry`, and `with_summary`. Middleware-attached
  audit metadata rides on the dispatch result `audit` field (free-form dict
  aligned with A2A `metadata` / ACP `kind` / OpenAI `summary_text` / OTel
  `gen_ai.tool.description` conventions) and is also fanned out as a new
  `tool_call_audit` AgentEvent for live ACP/A2A consumers. See
  [`docs/src/stdlib/tool-middleware.md`](docs/src/stdlib/tool-middleware.md)
  for the full reference.

- **`llm_caller` seam on `agent_loop`.** New option accepts a closure
  with the canonical shape `fn(call) -> {ok, value | status, error?}`
  that owns each turn's `llm_call(...)`. The loop validates the
  envelope shape, threads `_turn_iteration` through `call.turn`, and
  rejects writes to underscore-prefixed runtime-private keys.
- **`std/llm/handlers`.** Composable middleware: `default_llm_caller`,
  `with_retry`, `with_fallback`, `with_shadow`, `with_prompt_rewrite`,
  `with_logging`, `with_budget`, `with_cache`, `with_circuit_breaker`,
  `compose([...])`. `compose` takes a single list (Harn does not yet
  support user-defined variadics).
- **`std/llm/ensemble`.** `best_of_n`, `self_consistency`,
  `parallel_judge`, `debate`. Cites Wang et al. 2022
  (arxiv:2203.11171) and Du et al. 2023 (arxiv:2305.14325).
- **`std/llm/refine`.** `refine_prompt`, `refine_caller` —
  meta-prompt-based prompt rewriting with a `DIFF:` summary trailer.
  Best-effort session cache (Harn closures capture by value).
- **`std/llm/budget`.** `estimate_text_tokens` (heuristic; not named
  `estimate_tokens` to avoid colliding with the workflow builtin),
  `context_window_for`, `recommend_max_output_tokens`,
  `budget_summary`, `fits_in_context`.
- **`std/llm/defaults`.** `pack_for(opts)` and convenience wrappers
  (`pack_chat`, `pack_agent`, `pack_refine`, `pack_judge`,
  `pack_summarize`, `pack_code`, `pack_json`). Calibrated for
  Anthropic Sonnet/Opus/Haiku 4.x, OpenAI GPT-5/5.5/4o/4.1, Gemini
  2.5 Pro/Flash, Ollama Qwen3/Llama 3.x.
- **`std/llm/safe`.** `safe_call`, `safe_field`, `dict_get_ci`,
  `with_case_insensitive_keys`, `structured_envelope_or_default`,
  `judge_payload`, `verdict_normalize`, `schema_retry_nudge_for`.
- **`std/llm/prompts`.** `system_prelude`, `tool_use_prelude`,
  `structured_output_preface`.
- **`std/llm/catalog`.** `model_info(selector)`,
  `resolved_options(opts)`, `has_capability(model, cap)`,
  `family_of(model_id)`. Harn-side names are deliberately shorter than
  the underlying builtins to avoid shadowing.
- **New Rust builtins.** `llm_resolved_options(opts)`,
  `llm_model_defaults(model_id)`. `llm_model_info(model_id)` already
  existed and is reused. Implemented in
  `crates/harn-vm/src/llm/config_builtins.rs`.
- **`harn models list [--provider NAME] [--json] [--installed-only]`.**
- **`harn models install <model> [--yes] [--keep-alive VALUE]`.**
- **`harn try "<prompt>"`.** One-shot prompt against the configured
  provider with `with_retry`-wrapped `default_llm_caller`.
- **`harn doctor --json`.** Doctor now also checks Ollama, hardware,
  Harn version, and prints a "Next step" suggestion.
- **Orchestration hook dispatch benchmark.** Added `harn-orchestration-perf`
  and `make bench-orchestration` to measure no-op VM lifecycle hook fanout
  costs across 1, 8, 32, and 128 trigger-shaped events.
- **Lint rule `deprecated_llm_options`.** Warns on `llm_retries` /
  `llm_backoff_ms` in dict literals passed to `llm_call` /
  `llm_call_safe` / `llm_call_structured` / `llm_call_structured_result` /
  `agent_loop`.
- **`harn quickstart` setup wizard (#1331).** Added an interactive and
  non-interactive setup flow that detects provider credentials, local Ollama,
  free disk space, and local GPU availability, then writes starter
  `providers.toml`, `harn.toml`, and `.env` files.
- **Tiktoken-grade token counting.** Added `tiktoken_count_tokens(text, model)`
  and `std/llm/budget` helpers so budget checks use exact OpenAI tiktoken
  counts when available, labeled tiktoken approximations for Claude/Gemini
  model families, and the heuristic fallback only for unknown model IDs.

### Changed

- `harn doctor` output splits credentials and network checks.
- Improved no-providers-configured error message names every supported
  env var dynamically and points at `harn doctor` and (when available)
  `harn models recommend`.
- **`std/async` predicate retry rename.** Renamed `retry_with_backoff` to
  `retry_predicate_with_backoff` and removed the old export, with lint autofix
  support for stale call sites.

### Deprecated

- `llm_retries` and `llm_backoff_ms` on `llm_call` family + `agent_loop`
  — still functional in v0.8.x, removed in v0.9.0. Replace with
  `with_retry(default_llm_caller(), {max_attempts: K + 1})` from
  `std/llm/handlers`. `harn lint` warns on deprecated usage (rule:
  `deprecated_llm_options`).
- `llm_options` patch field on `post_turn_callback` outcomes —
  soft-deprecated; emits one event per session when used. Replace by
  wrapping `llm_caller` with a per-turn opts mutator (e.g.
  `with_prompt_rewrite`).

### Migration

`llm_retries: K` historically meant **K retries after the first
attempt** = K + 1 total attempts. `with_retry`'s `max_attempts: N`
counts **total attempts**. Adjust the number when migrating.

Before:

```harn,ignore
llm_call(prompt, sys, {llm_retries: 3, llm_backoff_ms: 250})
```

After:

```harn,ignore
import {default_llm_caller, with_retry} from "std/llm/handlers"

let caller = with_retry(default_llm_caller(), {
  max_attempts: 4,        // NOTE: total attempts; llm_retries: 3 == 4 attempts
  base_ms: 250,
  backoff: "exponential",
})
caller({prompt: prompt, system: sys, opts: {},
        turn: {iteration: 0, session_id: "", attempt: 1}})
```

Or pass through the new `llm_caller:` option on `agent_loop`:

```harn,ignore
agent_loop(task, sys, {
  llm_caller: with_retry(default_llm_caller(), {max_attempts: 4}),
})
```

## v0.7.62

### Added

- **Per-upstream LLM handler circuit pooling.** `std/llm/handlers` now provides
  `with_circuit_breaker`, which derives circuit names from each call's
  `(provider, model)` by default while preserving `name` for intentionally
  shared circuit state.
- **Persistent LLM response caching (#1322).** Added `std/cache` and
  `std/llm/handlers.with_cache` with sqlite and filesystem backends,
  content-addressed LLM keys, TTL, LRU eviction, and default bypass for calls
  that include tools.
- **Ollama provider config seeding.** `harn-cli` now automatically seeds Ollama
  provider configuration on the first LLM run, simplifying setup for new
  environments.
- **Prompt optimization helpers in `std/llm`.** New `judge`, `refine`, and
  `optimize` helpers support iterative prompt improvement flows with
  conformance coverage and docs.
- **Adaptive debate stopping for LLM ensembles.** `std/llm/ensemble` now
  supports fixed-round and adaptive stopping policies for multi-call ensemble
  workflows.
- **LLM reranking helpers.** Added `std/llm/rerank` and VM/provider plumbing for
  pairwise reranking workflows, including prompt assets, mocks, conformance
  coverage, and docs.
- **Shell completion generation.** Added `harn` shell completion generation
  support (bash, zsh, fish) to streamline CLI usage in interactive shells.
- **Chat project template.** New `harn init` chat project template provides a
  pre-configured starter for building chat-based agent applications.
- **Imports in `harn run -e`.** The `-e` flag now supports module imports,
  enabling more complex inline script composition via `harn run -e`.
- **Stream `harn run` stdout.** `harn run` now streams stdout to the terminal
  during execution, providing real-time feedback for long-running agent runs.
- **LLM options roundtrip benchmark.** Added a benchmark package for measuring
  LLM options serialization and package-verification coverage for the new perf
  crate.

### Fixed

- **Summary preset `tool_choice` default.** Fixed the default `tool_choice`
  behavior for summary presets in the agent stdlib, ensuring correct tool
  selection in audit summaries.

## v0.7.61

### Added

- **Adaptive iteration budgeting and `loop_control` policy on `agent_loop`.**
  `iteration_budget: {mode, initial, max, extend_by}` lets a loop start with a
  small cap and extend it transparently when there's evidence of progress;
  pass `loop_control: { state -> ... }` for a fully custom policy that sees a
  stable normalized loop-state snapshot (`budget`, `turn`, `session`,
  `completion`, `progress`) and returns `nil`, `{action: "extend"}`, or
  `{action: "stop"}`. `max_iterations` keeps working as a fixed cap. Decisions
  are surfaced under `result.adaptive_budget` and as `LoopControlDecision`
  events on the live event stream / ACP wire.
- **Generic role presets in `std/agent/presets`.** `agent_preset(kind,
  options?)`, `agent_budget(...)`, and `audit_agent` / `repair_agent` /
  `summary_agent` / `verify_agent` package the common harness shapes (audit /
  repair / summary / verify) so scripts don't have to hand-tune
  `max_iterations`, `max_nudges`, `done_sentinel`, `done_judge`, or
  `thinking`. Presets pick a provider-aware `thinking` mode (adaptive vs.
  effort vs. disabled) only when the caller hasn't set one, using
  `provider_capabilities` introspection.
- **`std/command` step runner.** New `stdlib_command.harn` provides a reusable
  command-step primitive for harness scripts, with conformance tests covering
  basic execution and retry logic.

### Fixed

- **Required tool completion gate.** `agent_loop` now treats unsatisfied
  `require_successful_tools` as an active completion gate: if the model tries
  to finish early, Harn injects feedback and continues while budget remains,
  and any terminal missing-required-tools state consistently returns
  `status = "failed"` / `stop_reason = "missing_required_tools"`.

## v0.7.60

### Fixed

- **Workflow LLM policy ceilings.** Fix an over-restrictive policy ceiling so that
  LLM operations such as `llm_call` and `agent_loop` require only the
  `llm.call` capability, not `side_effect=network`. Workflow stages with
  read-only, workspace-write, or process-exec tool ceilings can now call the
  configured LLM. Tool-derived policies with no capability annotations now leave
  capability ceilings unspecified instead of narrowing custom node capabilities.

## v0.7.59

### Added

- **Categorical timing breakdown for `harn run`.** Added `--profile` support that
  reports categorical performance metrics, including a new `profile` crate and
  CLI integration for detailed runtime insights.
- **`pr-finish-pass` skill and slash command.** Introduced a new skill and
  associated slash commands for Claude Code and Codex to streamline PR
  finishing workflows.
- **Agent loop policy scoping to tool dispatch.** Refined the Harn agent loop
  to scope policies specifically to tool dispatch, adding conformance tests
  for loop unwinding, prefill feedback, and worker policies.

### Changed

- **Agent loop policy enforcement.** Updated the Harn stdlib and VM orchestration
  to enforce agent loop policies during tool dispatch, improving safety and
  control over agent behavior.

## v0.7.58

### Added

- **Persona-aware crystallization proposals.** `std/personas/prelude` now
  includes `persona_crystallization_candidates(...)` and
  `persona_crystallization_bundle(...)` for mining successful repair-worker
  receipts after the hosted-history gate. The helper emits the existing
  `harn.crystallization.candidate.bundle` shape with matched traces, shadow
  comparison, savings estimates, and a literal Harn `@step` patch for persona
  package review.
- **Skill provenance endorsement chains.** Skill signatures now use
  `harn-skill-sig/v2` with an author signature plus one or more trusted
  endorsement signatures. Added `harn skill endorse`, `harn skill who-signed`,
  Harn-visible `skill_who_signed(...)` registry queries, and transcript
  metadata that exposes signer trust-policy inputs for `trust.query`.
- **Merge Captain cheap-model prompt pack.** Added a Harn-native value-model
  prompt pack with narrow prompts for PR classification, deterministic action
  choice, CI diagnosis, repair summaries, approval decisions, and release
  changelog audit/rewrite. The pack ships strict JSON schemas, golden examples,
  compact context budgets that exclude raw logs unless selected spans are
  provided, and revision gates for transcript-oracle diffs plus
  timeout-ladder results.
- **ACP authentication flow.** `harn serve acp` now advertises configured
  `authMethods`, implements ACP `authenticate`, and returns `auth_required`
  before protected session methods until the connection authenticates.
- **A2A canonical HTTP+JSON/REST binding.** The A2A server now exposes the
  spec-blessed REST surface under `/v1` (`POST /v1/message:send`, `POST
  /v1/message:stream`, `GET /v1/tasks/{id}`, `POST /v1/tasks/{id}:cancel`,
  `POST /v1/tasks/{id}:subscribe`, push-notification-config CRUD under
  `/v1/tasks/{id}/pushNotificationConfigs`, and `GET /v1/card`). The agent card
  advertises both `JSONRPC` and `HTTP+JSON` transports in `additionalInterfaces`.
  The previous non-canonical paths (`/message/send`, `/message/stream`,
  `/tasks/send`, `/tasks/send_and_wait`, `/tasks/cancel`,
  `/tasks/resubscribe`) keep working for one minor cycle and now emit a
  `Deprecation` header pointing at the canonical replacement.
- **MCP logging notifications.** `harn mcp serve` now forwards Harn's structured
  audit and observability event-log streams (signature-verify, secret-scan,
  egress, trigger operations, DLQ, action graph) as MCP `notifications/message`
  per the MCP logging spec. `logging/setLevel` is now honored per session and
  filters notifications by severity; events can override the assigned level via
  a `severity` header.
- **MCP `notifications/progress` from long-running tools.** Tool handlers can
  call the new `mcp_report_progress(progress, opts?)` builtin to emit
  `notifications/progress` updates for the in-flight call. The builtin no-ops
  when the client did not opt in via `_meta.progressToken`, so scripts can
  sprinkle it without preflight checks. Both stdio and HTTP MCP transports
  thread the token through to script-defined tools (`mcp_tools`) and
  exported-function tools (`harn serve mcp SCRIPT`). The orchestrator-mode
  `harn.trigger.fire` tool also emits its own milestones (loading runtime,
  preparing event, firing trigger, complete).

### Changed

- **Agent loop orchestration core.** Overhauled the agent orchestration core,
  moving sub-agent request shaping into Harn stdlib and wire-turn/judge events,
  tool_format claim, and option validation. The agent loop now honors post-turn
  options in the stuck guard and exposes Merge Captain persona steps. Client
  tool search is moved into stdlib, and workflow stage execution primitives are
  split for better modularity.
- **Agent prompt overrides.** Tightened agent prompt overrides to ensure
  consistent behavior across sessions. Updated prompt assets and validation
  logic in the VM to reflect stricter constraints on tool contracts and native
  completions.
- **Persona steel-thread conformance.** Added a conformance harness for persona
  steel-thread scenarios, including deploy, merge, and release captain tests.
  This ensures persona lifecycle hooks and autonomy tiers are enforced
  correctly under VM constraints.
- **VM and stdlib refactoring.** Moved remaining Rust-owned prompt prose into
  stdlib assets, deleted legacy Rust agent orchestration modules, and wired
  per-agent permissions through the Harn agent loop. The parse guidance prose is
  now in a prompt asset, and workflow stage options are moved into Harn.

### Fixed

- **Ollama tool history replay.** Fixed issues with Ollama tool history replay
  to ensure accurate state restoration during agent turns.
- **Native tool history replay.** Fixed native tool history replay to maintain
  consistency in tool execution traces.
- **Harn orchestration boundary.** Polished the Harn orchestration boundary to
  ensure clean separation between the VM and stdlib components.

## v0.7.57

### Added

- **Persona orchestration prelude.** New `std/personas/prelude` helpers provide reusable,
  receipt-friendly workflow patterns: `verify_then_act`, `bounded_loop`,
  `cheap_classify_then_escalate`, `parallel_sweep_with_circuit_breaker`,
  `with_audit_receipt`, and `with_approval_gate`. The helpers share a structured
  `{ok, status, result, error, receipt}` envelope and are documented in
  `docs/src/personas/prelude.md`.
- **A2A push notification auth.** The A2A adapter now verifies inbound push webhooks and can
  register authenticated outbound push callbacks. Supported callback schemes include Bearer,
  Basic, API key, OAuth2 client credentials, OpenID Connect ID tokens with JWKS validation,
  and mTLS.
- **MCP resource subscriptions.** Harn MCP servers now advertise and handle
  `resources/subscribe` and `resources/unsubscribe`, including subscription tracking and
  update notifications for resource-aware clients.
- **Generic `agent_turn` wrapper and sentinel judge.** New `agent_turn(prompt, opts?)`
  wraps the single-turn agent pattern with progress guidance, completion-sentinel judging,
  and compact iteration/judge summaries. `agent_loop` also accepts `done_judge: true` or a
  judge policy dict.
- **Type-aware `unnecessary-safe-navigation` diagnostics.** The typechecker now warns and
  offers fixes when `?.`, `?.method(...)`, or `?[]` is used on a receiver known to be
  non-optional.
- **List literals in ternary branches.** Ternary arms can now be list literals without
  extra grouping, e.g. `condition ? [a, b] : [c]`.
- **`unnecessary-parentheses` lint.** `harn lint` now warns on parentheses around a single
  value expression when those parentheses are not required by call, declaration, or
  precedence context.

### Changed

- **Unified static and runtime typechecking.** Builtin signatures now flow through one
  parser-side registry for static checks and VM call-boundary validation. This tightens
  arity/type behavior and removes legacy drift between compile-time and runtime errors; code
  that relied on missing arguments being silently treated as `nil` should make defaults
  explicit.
- **Canonical `harn-stdlib` crate.** Standard-library `.harn` sources moved out of
  `harn-modules` into a new dependency-free `crates/harn-stdlib` catalog crate. Public
  `std/...` imports continue to resolve, while `harn-modules` and `harn-vm` delegate source
  lookup to the shared catalog.
- **ACP agent event coverage.** Agent loop lifecycle events such as `BudgetExhausted`,
  `LoopStuck`, and `DaemonWatchdogTripped` are now bridged as `_harn/agentEvent`
  `ExtNotification`s with advertised capability metadata.
- **CI sweep placement.** Heavy flake-detection and thread-parity sweeps moved off the PR
  path to scheduled/manual workflows, while the retained PR checks keep the faster smoke and
  portability coverage.

### Fixed

- **Tool-surface alias warnings.** Deprecated argument alias diagnostics are scoped to the
  matching tool call instead of scanning all prompt text, eliminating false positives across
  unrelated tool surfaces.
- **Release binary workflow idempotency.** `build-release-binaries.yml` now runs on tag push
  or manual dispatch and checks existing release assets/container tags before rebuilding,
  with a `force_rebuild` escape hatch for recovery.
- **Windows and generated-text portability in CI.** The release-window CI changes also fixed
  path separator, CRLF, and generated text comparison issues in the retained Windows smoke
  and spec sync jobs.

## v0.7.56

### Added

- **ACP wire-format coverage for the agent-event tail.** Previously six
  `AgentEvent` variants (`TurnStart`, `TurnEnd`, `FeedbackInjected`,
  `BudgetExhausted`, `LoopStuck`, `DaemonWatchdogTripped`) silently fell off
  the bridge because they had no matching `SessionUpdate` discriminator and
  the strict deserializer would reject custom kinds. The ACP adapter now
  emits them as spec-blessed `_harn/agentEvent` `ExtNotification`s with
  pinned wire fixtures, and the `agentCapabilities._meta.harn` block
  advertises the method + the six event kinds so external consumers can
  feature-detect.
- **`verify_completion` agent-loop stop hook.** New `agent_loop` option
  takes a closure `{ info -> nil | "feedback" }` that runs the moment the
  loop wants to yield. Returning `nil` / `true` confirms the stop;
  returning a string vetoes it, injects the feedback as a runtime
  message, emits a `FeedbackInjected` event, and continues the loop.
  Capped by `max_verify_attempts` (default 3) which surfaces as
  `status: "verify_exhausted"` when reached. The `info` payload includes
  `session_id`, `iteration`, `stop_reason`, `last_text`, and per-session
  tool-use counts so judge logic can fork transcripts via
  `agent_session_fork_at` + `parallel settle` without any new runtime
  primitives.
- **Built-in `verify_completion_judge` stop policy.** Hosts can now pass
  `verify_completion_judge: true` or a policy dict to have Harn own the
  structured completion-judge side call, transcript rendering, feedback
  injection, optional final-response synthesis, and bounded retry loop.
  The schema requires only `pass`; `feedback` and `final_response` are
  optional and may be `null`, so weak local models do not need to emit
  placeholder strings on passes.
- **`post_turn_callback` enriched payload.** The post-turn callback now
  receives `session_id` so callbacks can address the live session, and
  the verdict shape was extended with an `injects: [{role, content}]`
  list for typed message injection alongside the legacy `message: "..."`
  feedback nudge.
- **Capability-driven `thinking_disable_directive`.** A new
  `[[provider.<name>]]` capability (`thinking_disable_directive`) lets
  the matrix declare an in-prompt thinking-off directive (e.g.
  `"/no_think"` for Qwen3 chat templates). When `thinking: false` is
  requested for a model whose capabilities row sets it, Harn auto-prepends
  the directive to the system message, idempotently. Scripts now write
  `thinking: false` once and have it work uniformly across Anthropic /
  OpenAI / Qwen3 without per-template prompt knowledge. Shipped on every
  Qwen3 row: ollama, llamacpp, local, mlx, dashscope, fireworks,
  openrouter, huggingface, together.

### Changed

- **`done_sentinel` accepts a first-class `bool` and defaults are
  `tool_format`-aware.** Old contract forced users to write
  `done_sentinel: ""` everywhere — even on native-tool models that
  signal completion by simply not calling a tool — to suppress a
  `##DONE##` injection that didn't belong there. New contract:
  - `done_sentinel: true` → explicit `##DONE##` (legacy default).
  - `done_sentinel: false` → explicit disable.
  - `done_sentinel: "..."` → custom sentinel.
  - omitted → defaults to disabled when `tool_format == "native"`,
    otherwise `##DONE##`.
  Both `agent_loop` parser paths (`agent_config.rs` and `mod.rs`) route
  through the same shared parser; previously only one accepted the bool
  shape, which silently drained `done_sentinel: true` to `None` in
  paths that hit the legacy `mod.rs` path. Wrong-type values
  (`done_sentinel: 42`) now error loudly at parse time.
- **`post_turn_callback` / `verify_completion` parse strictly.** Both
  options previously used `.filter(|v| matches!(v, Closure(_)))` which
  silently dropped non-closure values to `None` — typos in option keys
  or value types went unnoticed. They now error
  (`'<name>' must be a closure or nil; got <type>`) so foot-guns surface
  immediately.
- **Completion verification is conservative under judge failure.** The
  built-in judge now treats malformed or schema-invalid structured
  output as a veto with fallback feedback instead of confirming
  completion. `max_verify_attempts` now defaults to 20 so product
  integrations can safely allow long local-model recovery loops while
  still capping infinite judge-veto cycles.
- **Ollama native tool history replay.** Replayed assistant tool-call
  history now uses the OpenAI-compatible string-encoded
  `function.arguments` shape that modern Ollama validates, avoiding
  second-iteration local Qwen failures after a native tool call.

- **Merge Captain timeout ladder evals (#1014).** Added a reusable persona eval ladder runner for
  Merge Captain, exposed through `harn merge-captain ladder`, `harn eval`, `harn test package
  --evals`, and `persona_eval_ladder_*` Harn builtins. Ladder reports capture per-tier transcripts,
  receipts, cost, latency, tool/model counts, state-machine coverage, first correct tier, and
  degraded or looping tiers; `personas/merge_captain/harn.eval.toml` ships a Gemma value-model
  profile.

- **Merge Captain fake GitHub/fake git conformance suite (#1012).** Expanded the deterministic
  Merge Captain fixture pack with pending-check, clean-rebase, merge-queue entry, merge-group
  failure, failing-CI repair, new-PR-arrival, downstream version-bump, force-with-lease mismatch,
  and release-harness crystallization scenarios. Goldens can now pin exact state-transition
  sequences so CI catches drift before a captain touches live repos.

- **ACP mode config options follow the current protocol (#897).** The ACP adapter now exposes
  session modes through preferred
  [session config options](https://agentclientprotocol.com/protocol/session-config-options) while
  keeping legacy [session-modes](https://agentclientprotocol.com/protocol/session-modes) in sync.
  Sessions default to conservative `ask`, advertise `ask`, `architect`, `code`, and `shadow`, and
  accept both `session/set_config_option` and `session/set_mode`. Mode changes emit both
  `current_mode_update` and `config_option_update`, and non-`code` modes derive their prompt-time
  capability ceiling from Harn's `AutonomyTier` policy.
- **Replay-for-teaching corrections (#932).** `harn trigger replay` now accepts
  `--steer-from` with a human-provided `--to-decision` and records the divergence
  as a typed correction. The new `std/corrections` API exposes correction
  recording/query helpers, and persona/all-scope corrections tighten derived
  capability policy for the affected actor.

## v0.7.55

### Added

- **Typed GitHub trigger payloads for the new connector contract (#1158).** Promoted five additional
  `GitHubEventPayload` variants — `GitHubCheckSuiteEventPayload`, `GitHubStatusEventPayload`,
  `GitHubMergeGroupEventPayload`, `GitHubInstallationEventPayload`, and
  `GitHubInstallationRepositoriesEventPayload` — so `harn-github-connector` v0.2.0 deliveries no
  longer collapse into the catch-all `Other` variant. Every variant exposes the connector's promoted
  scalars at the top level of `event.provider_payload`. Bumped the connector pin from v0.1.0 to
  v0.2.0.

- **Release-harness fixture ingest (#1146).** Added `harn crystallize ingest --from <FIXTURE_DIR>
  --bundle <BUNDLE_DIR>` to consume a `release_harn.crystallization_input.v1` fixture and produce a
  reviewed crystallization candidate bundle. The emitted bundle uses the existing
  `harn.crystallization.candidate.bundle` schema. A sample fixture lives at
  `crates/harn-vm/tests/fixtures/release_harn_sample/`.

- **Merge Captain repair-worker checkpoint contract (#1010).** Added a typed contract for the
  bounded `agent_loop` worker with modules under `personas/merge_captain/lib/` covering bundle
  preparation, per-action approval gates, the dispatcher, and deterministic output validators.
  Workers produce a versioned `merge_captain.repair_run` record linked back via a compact summary.

- **A2A `input-required` / `auth-required` / `rejected` task states (#889).** The A2A adapter's
  `TaskStatus` enum now covers all six A2A 0.3.0 non-final/final state strings. `input-required` is
  wired into HITL primitives; synchronous policy denial lands the task in `rejected`; auth errors
  flip to `auth-required`.

- **Per-step model routing + token / cost budgets (#1074).** `@step` now accepts `model: "..."` and
  `budget: { max_tokens: N, max_usd: N.NN }` so a persona can mix cheap classification (Haiku),
  semantic judgment (Sonnet), and frontier escalation (Opus). Per-step token/cost spend is tracked
  separately; errors honor the step's `error_boundary` (`fail`, `continue`, or `escalate`).

- **MCP `sampling/createMessage` server-to-client bridge (#874).** When Harn acts as an MCP client,
  it now declares the `sampling` capability and accepts inbound `sampling/createMessage` requests
  from connected servers. Requests flow through `llm_call`'s typed boundary; without a bridge,
  sampling requests are declined with `mcp.samplingDeclined`.

### Changed

- **CHANGELOG retroactive-edit guard.** Added `scripts/check_changelog_no_retroactive_edits.py`,
  wired into CI and the pre-push hook. The check fails when a released section is modified; new
  entries belong under `## Unreleased`. Bypass with `ALLOW_CHANGELOG_RETROACTIVE_EDIT=1`.

### Fixed

- **Stop pre-existing MCP/long-running test flakes (#1162).** Addressed flaky tests in the MCP and
  long-running test suites.

- **Ensure final prose after text tool actions (#1163).** Ensured that prose output follows text
  tool actions correctly.

- **Use capability matrix for native tool mode (#1159).** Adopted the capability matrix as the gate
  for native tool mode decisions.

- **Lock agent sessions to one tool format (#1155).** Constrained agent sessions to a single tool
  format to prevent cross-format conflicts.

- **fix(tree-sitter): unblock release audit's strict parse sweep (#1149).** Resolved tree-sitter
  issues that were blocking the release audit's strict parsing.

- **Publish OpenTrustGraph v0 as a portable open spec (#930).** Published the OpenTrustGraph v0
  specification artifact.

- **docs(adr): record compile-time capability invariants decision (#925).** Documented the
  compile-time capability invariants decision in the ADR archive.

## v0.7.54

### Added

- **Per-call `agent_loop` transcript directories (#1145).** New `llm_transcript_dir`
  option on `agent_loop`, `sub_agent_run`, and workflow model-policy paths lets
  individual Harn workflows write auditable LLM JSONL transcripts without relying
  solely on the global `HARN_LLM_TRANSCRIPT_DIR` environment variable. When
  `nil`, the environment variable remains the fallback.

- **Compact text-tool protocol tag support (#1144).** Harn now accepts compact
  text-tool aliases such as `<toolcall>`, `<assistantprose>`, and `<userresponse>`
  emitted by local models (e.g. llama.cpp, Qwen). These are canonicalized back to
  the standard underscore-prefixed tags for stored history, and the streaming
  detection correctly emits normal tool-call lifecycle events.

- **MCP tool tasks (#1143).** Harn MCP servers now advertise and handle the MCP
  2025-11-25 experimental tasks surface: `tasks/get`, `tasks/result`,
  `tasks/list`, `tasks/cancel`, and `notifications/tasks/status`. Task-augmented
  `tools/call` is supported for `harn.trigger.fire`, `harn.trigger.replay`, and
  `harn.orchestrator.dlq.retry` with inline execution for non-task-capable tools.
  Task IDs are server-side UUID v7; the task store is in-memory per server process.

- **Durable A2A push notification config CRUD (#1142).** The A2A adapter now
  persists push notification endpoint configuration (`set`, `get`, `list`,
  `delete`) on the shared EventLog, with replay at server start so configs
  survive restarts independently of live in-memory task state. Protocol
  conformance fixtures cover the canonical CRUD shapes.

### Changed

- **Diagnostic paths normalized in error output (#1141).** Parser and runtime
  error messages now cancel `.` and `..` path components in file references,
  so errors like `mode/../lib/runtime/loop.harn` render as `lib/runtime/loop.harn`.
  Source metadata for debugging is preserved.

### Fixed

- **Compact protocol tags no longer leak into visible text (#1144).** Models
  emitting compact text-tool tags (e.g. `<toolcall>`) without underscores no
  longer cause visible-text sanitization to expose raw protocol text or suppress
  tool-call events.

## v0.7.53

### Added

- **MCP `sampling/createMessage` server-to-client bridge (#874).** When
  Harn acts as an MCP client (stdio or Streamable HTTP), it now declares
  the `sampling` capability on `initialize` and accepts inbound
  `sampling/createMessage` requests from connected servers. Each request
  is dispatched through the host-call bridge as
  `("mcp", "sample", {server, params})` so embedders can run their own
  approval UX, rate-limit by server, or inject `llm_call` overrides
  (e.g. force `provider`/`model`). On approval the request flows through
  Harn's regular `extract_llm_options` + `execute_llm_call` boundary —
  picking up routing, capability gates, mock interception, and budget
  plumbing — and the response is returned to the originating server as
  `{role: "assistant", content: {type: "text", text}, model, stopReason}`.
  Without an installed bridge, sampling requests are declined with a
  structured `mcp.samplingDeclined` error (code `-32603`) so servers can
  fall back to a sensible default rather than driving unattended LLM
  spend. Sampling-side `tools` / `toolChoice` / `thinking` /
  `modelPreferences` / `stopSequences` / `metadata` are all forwarded
  through to `llm_call`'s typed boundary so the existing ThinkingConfig
  (#849) and structured-output paths apply unchanged.
- **ACP session modes (#897).** The ACP adapter now implements the
  [session-modes](https://agentclientprotocol.com/protocol/session-modes)
  spec: `session/new` and `session/load` return a `SessionModeState`
  (`{ currentModeId, availableModes }`) describing the four-mode catalog
  (`default`, `architect`, `code`, `ask`); `session/set_mode` switches
  the active mode and emits a `current_mode_update` notification; and
  `session/fork` carries the parent's mode over to the new branch.
  `architect` and `ask` modes push a read-only capability ceiling onto
  the VM execution stack while the prompt runs, so destructive builtins
  (`write_file`, `exec`, network calls, etc.) are rejected before they
  touch the workspace. `default` and `code` preserve the pre-modes
  baseline.

- **A2A authenticated extended agent card (#888).** `harn serve a2a` now
  honors the A2A 0.3.0 contract for `agent/getAuthenticatedExtendedCard`.
  Public agent cards advertise `supportsAuthenticatedExtendedCard: true`
  (and `capabilities.extendedAgentCard: true`) when the server is
  configured with at least one `AuthPolicy` method, otherwise both flags
  remain `false`. The RPC method enforces the configured auth schemes:
  unauthenticated calls return HTTP 401 with a `WWW-Authenticate`
  challenge listing the supported schemes (`Bearer` for API key/OAuth,
  `HMAC-SHA256` for HMAC), while authenticated callers receive an
  extended card with `metadata.extendedAgentCard: true`, the resolved
  principal subject, declared `securitySchemes`, and per-skill
  `outputSchema`. When no auth is configured the server returns
  `ExtendedAgentCardNotConfiguredError` (`-32007`) per the spec.

- **Per-agent autonomy budget for `agent_loop` (#928).** New
  `autonomy_budget: {per_hour, per_day, key?, reviewer?}` option on
  `agent_loop` (and downstream `sub_agent_run` / workflow stages) caps
  autonomous decisions per UTC hour / day per stable key. The check
  fires at loop entry, before any LLM/MCP work — scripts can't bypass
  it. When exhausted, `agent_loop` returns
  `{status: "approval_required", request_id, reviewers, reason, ...}`,
  appends a HITL approval request to `hitl.approvals`, emits an
  `autonomy.budget_exceeded` lifecycle event, and writes an
  `autonomy.tier_transition` trust-graph record from `act_auto` to
  `act_with_approval` — the same audit trail the trigger-side
  `max_autonomous_decisions_per_*` cap produces.

- **MCP elicitation (`elicitation/create`) on both roles (#875).** Harn
  servers can now prompt connected clients for structured user input
  mid-tool-call via the `mcp_elicit({ message, requestedSchema })`
  builtin, returning the canonical `{action, content?}` envelope per
  MCP 2025-11-25. `content` is validated against `requestedSchema`
  before returning. The MCP server's `initialize` response now
  advertises the `elicitation` capability. On the client side, inbound
  `elicitation/create` requests are dispatched to the embedder via the
  `HostCallBridge` (`capability="mcp"`, `operation="elicit"`); if no
  host bridge is wired up, Harn responds with `{ action: "decline" }`.
  Bidirectional stdio is implemented; HTTP streamable-transport server
  surfaces are wired through the per-session SSE stream so a tool
  handler can elicit and receive a response over a separate POST.

- **ACP slash-commands via `@command` (#896).** New `@command(name?,
  description?, hint?)` attribute on top-level pipelines. The ACP adapter
  (`harn serve --pipeline ...`) discovers tagged pipelines from the
  loaded source, advertises them via `available_commands_update` after
  `session/new` and on hot-reload (re-emitted when the source changes
  between prompts), and dispatches `/<name> args` prompts to the named
  pipeline as the entry point with `args` exposed as the `prompt`
  global. Implements ACP slash-command spec for Zed-style clients.

- **Postfix `T?` optional-type sugar (#915).** `T?` now parses as
  syntactic sugar for `T | nil`, mirroring the safe-navigation postfix
  already used in expressions (`obj?.method()`). Postfix `?` binds
  tighter than `&` and `|`, so `A & B?` parses as `A & (B | nil)` and
  `A | B?` flattens to `A | B | nil`. Equivalent narrowing rules apply
  (`if x != nil` still narrows away the `nil` arm). The formatter
  rewrites the explicit `T | nil` form to `T?` whenever the inner type
  prints as a primary, and a new `prefer-optional-shorthand` lint rule
  (with auto-fix) flags the long form independently. Tree-sitter
  grammar, `spec/HARN_SPEC.md`, and conformance tests cover both
  spellings; existing `T | nil` source remains valid and semantically
  identical.

- **Merge Captain end-to-end driver (#1019/#1115).** Adds
  `harn merge-captain run --backend {mock,replay,live}` with model-route
  and timeout-tier receipt pins, JSONL transcript streaming, persisted
  receipts, run summaries, and a checked-in mock smoke scenario.

- **Merge Captain mock-repos playground (#1020/#1137).** New
  `harn merge-captain mock {init,step,status,serve,cleanup,scenarios}`
  subcommand tree materializing a real on-disk sandbox: bare + working git
  repos seeded from a checked-in scenario manifest plus a fake GitHub HTTP
  server (`pulls`, `pulls/.../merge`, `commits/.../check-runs`,
  `actions/runs/.../logs`, `merge_queue/queues/...`, `issues`,
  `issues/.../comments`, `issues/.../labels`). Step actions (`set_check`,
  `add_pull_request`, `merge_pull_request`, `force_push_author`,
  `advance_base`, `set_labels`, `set_merge_queue_status`,
  `set_mergeability`, `advance_time_ms`) flip state declaratively, with
  the git-native ones producing real commits on the bare remote so the
  captain's rebase / force-with-lease / merge-queue codepaths run
  unchanged. Ships three built-in scenarios (`three_repo_basic`,
  `single_green`, `force_push_drill`) and extends `--backend mock` to
  detect the on-disk playground and synthesize a canonical sweep
  transcript from the live state.

- **A2A file and data parts (#1111).** The A2A adapter now accepts and
  emits the `file` and `data` part variants from the A2A 0.3 spec
  alongside `text`, with conformance fixtures covering both
  `task.send`/`task.send_subscribe` ingest and downstream task/stream
  message rendering.

- **Pipe placeholder chaining (#1050).** New `_` placeholder syntax in
  pipe expressions lets scripts route the piped value into a non-first
  argument position (`x |> f(a, _, c)`), with parser, type inference,
  formatter, tree-sitter, and conformance coverage. Existing
  no-placeholder pipes preserve their first-argument semantics.

- **Typed git stdlib receipts (#1054).** `git_*` builtins now return
  structured `GitReceipt` envelopes with audit-grade fields (subprocess
  args, exit code, stdout/stderr digest, repo HEAD before/after) instead
  of bare strings, feeding the canonical receipt envelope (#1110).

- **TrustGraph query API (#1051).** Read-side query interface for the
  trust graph, exposing tier/edge/record lookups via
  `trust_graph_query(...)` and friends so policy-shaped scripts can
  reason about prior tier transitions and approvals.

- **PDF and audio file content support (#1046).** `llm_call` content
  blocks now accept PDF and audio attachments where the provider
  capability matrix supports them, alongside the existing image/text
  content blocks.

- **Durable persona annotations (#1055).** Persona definitions can carry
  annotations that survive transcript compaction, replays, and mode
  switches, feeding a `@durable_persona` attribute and conformance
  coverage in `attributes_durable_persona.harn`.

- **Persona step attribute metadata (#1117).** New `@step` attribute
  (with optional `label`, `owner`, `contract` fields) on pipeline steps
  surfaces structured metadata to ACP/UI consumers and conformance
  coverage in `attributes_step.harn`.

- **MCP OAuth resource-server auth (#1114).** Harn's MCP HTTP server can
  authenticate clients via OAuth resource-server flow with bearer-token
  validation and per-session capability scoping.

- **Canonical receipt envelope (#1110).** Standardizes the receipt
  envelope shape across stdlib + bridge + connector receipts and adds a
  `check_receipt_struct_duplication` script gate to prevent drift.

### Changed

- **A2A `a2a-version` request-header negotiation soft-deprecated (#894).**
  The A2A 0.3.0 spec encodes protocol version in AgentCard discovery, not
  request headers. The harn-serve A2A adapter no longer rejects unknown
  values of the `a2a-version` HTTP header with JSON-RPC `-32009
  VersionNotSupportedError`; clients should read `protocolVersion` from
  the AgentCard and choose compatible methods. For one minor cycle, any
  request that still carries `a2a-version` is logged via
  `tracing::warn!(target = "harn_serve::a2a", …)` so we can spot
  residual client usage; the header is then slated for full removal.

- **ACP vendor-extension session-update payloads follow `_meta.harn`
  (#905).** Harn-specific session-update variants now ride their vendor
  fields under `update._meta.harn` rather than the update root,
  completing the namespacing pass started in #904. Affected variants:
  `progress` (`phase`, `message`, `progress`, `total`, `data`), `log`
  (`level`, `message`, `fields`), `fs_watch` (`subscriptionId`,
  `events`), `worker_update` (`workerId`, `workerName`, `workerTask`,
  `workerMode`, `event`, `status`, `terminal`, `metadata`, `audit`),
  `transcript_compacted` (`mode`, `strategy`, `archivedMessages`,
  `estimatedTokensBefore`, `estimatedTokensAfter`, `snapshotAssetId`),
  `handoff` (`handoffId`, `artifactId`, `handoff`), `skill_activated` /
  `skill_deactivated` / `skill_scope_tools` (`skillName`, `iteration`,
  `reason`, `allowedTools`), and `tool_search_query` /
  `tool_search_result` (`toolUseId`, `name`, `query`, `promoted`,
  `strategy`, `mode`). Content extensions `visible_text` /
  `visible_delta` on `agent_message_chunk` move under
  `content._meta.harn`. The canonical `sessionUpdate` discriminator and
  the ACP `content` block stay at their top-level locations. Burin Code
  and other ACP hosts must migrate to the new field locations before
  consuming this release; pre-#905 fixtures will fail to render. Burin
  Code consumer migration is tracked in
  [burin-code#511](https://github.com/burin-labs/burin-code/issues/511).

- **ACP tool-call extension metadata follows `_meta.harn` (#904).**
  Harn-specific `tool_call` / `tool_call_update` fields (`audit`,
  `durationMs`, `executionDurationMs`, `error`, `errorCategory`,
  `executor`, `parsing`, and `rawInputPartial`) now live under
  `_meta.harn` instead of the ACP update root. Canonical fields such as
  `toolCallId`, `title`, `kind`, `status`, `rawInput`, and `rawOutput`
  remain top-level. Burin Code consumers should migrate to the new
  location before consuming this release.

- **Strip `GIT_*` env from `stdlib.git` subprocess calls (#1121).**
  `git_*` builtins no longer inherit ambient `GIT_*` environment
  variables from the parent process; subprocess invocations run with a
  scrubbed environment to prevent accidental cross-contamination from
  `GIT_DIR`, `GIT_WORK_TREE`, `GIT_AUTHOR_*`, etc.

- **CI rollup gate renamed: `Check status` → `CI status` (#1124).** The
  required-check rollup name now matches the org-level branch protection
  ruleset so PRs reliably enter the merge queue.

- **Pre-0.6 changelog archived (#1047).** Pre-0.6 release notes moved to
  a separate [`CHANGELOG-pre-0.6.md`](CHANGELOG-pre-0.6.md) archive; the
  main `CHANGELOG.md` now focuses on 0.6+ history.

- **Split `crates/harn-cli/src/cli.rs` by subcommand group (#942).** The
  monolithic CLI argument-parsing module is split into per-subcommand
  files for maintainability; behavior is unchanged.

### Fixed

- **Flaky `file_backed_prompts_list_render_and_notify_changes` test
  (#1125).** Removes wall-clock dependence in the file-backed prompts
  test by using deterministic notification ordering.

### Tests

- **`ProcessHandle` trait + `MockProcess` for deterministic
  process-tool tests (#1062).** `crates/harn-hostlib/src/process/`
  introduces a `ProcessHandle` / `ProcessSpawner` abstraction with a
  real implementation that wraps `harn_vm::process_sandbox` and a
  `MockSpawner` test double that returns scripted `MockProcess`
  handles. `tools/proc.rs` and `tools/long_running.rs` consume the
  trait instead of `std::process::Child` directly. The 33 integration
  tests in `crates/harn-hostlib/tests/process_tools.rs` are rewritten
  to install `MockSpawner` per test — zero `std::thread::sleep`, zero
  `Instant::now()` polling, zero real subprocess spawning. Long-running
  waiter completion is awaited deterministically via a new
  `register_completion_notifier` API. The full file finishes in 0.01 s
  and 50× rerun under `cargo nextest` is flake-free. Two real-process
  smoke tests live in `crates/harn-hostlib/tests/process_tools_e2e.rs`
  for end-to-end coverage; they keep the `_e2e.rs` suffix so issue
  #1069's slow-job tagging can pick them up.

- **`OrchestratorHarness` + in-process orchestrator integration tests
  (#1059/#1060/#1081/#1098).** Every `crates/harn-cli/tests/orchestrator_*`
  test now runs the orchestrator in-process via the new
  `OrchestratorHarness` library API and waits on the event log's
  broadcast channel instead of spawning the `harn` binary and polling
  SQLite. Wall-clock for the orchestrator HTTP suite drops from minutes
  (per-test subprocess startup + 25 ms-poll drain) to ~13 s end-to-end
  at `--test-threads=8`. The few tests that inherently depend on
  subprocess semantics (raw stderr scraping, `std::process::exit(86)`
  crash hooks, global tracing-subscriber install) stay `#[ignore]`d and
  move to the slow E2E/smoke job tracked in #1069. To support this the
  harn-cli crate gains a thin `lib.rs` that promotes the previous
  `main.rs` body to public API while keeping `main.rs` a 3-line shim.

- **`FakeLlmProvider` for deterministic LLM tests (#1113).** Replaces
  ad-hoc HTTP-mock-based LLM provider doubles with a synchronous
  in-process fake that scripts request/response sequences without
  binding tests to wall-clock or socket lifecycle.

- **`FakeHttpServer` to replace mock HTTP server (#1112).** Deterministic
  in-process HTTP server replaces the prior wall-clock-coupled mock,
  removing flake from connector and webhook tests.

- **Test suite tiering (#1087).** Splits the test suite into a fast
  push-hook gate and a slow E2E job; routine PRs no longer pay the
  subprocess-heavy E2E cost on every push.

- **CI determinism gates (#1085).** Adds flake-detection
  (`.github/workflows/flake-detection.yml`) and thread-parity
  (`.github/workflows/thread-parity.yml`) workflows that flag wall-clock
  or thread-count-dependent test behavior as part of merge-queue gates.

- **Lint test patterns (#1090).** New `make lint-test-patterns` gate
  bans `std::thread::sleep`, `tokio::time::sleep`, `Instant::now()`
  polling loops, `SystemTime::now()`, and short `recv_timeout` calls in
  test files; opt-out procedure documented in `docs/dev/testing.md`.

- **Deterministic timing migration.** Sweeping conversion of orchestrator
  (#1098), workflow (#1105), Slack 3 s-ack (#1102, #1104), A2A dispatcher
  (#1100), Notion/Linear connectors (#1086), CLI persona/flow ship
  watch (#1133), CLI in-process (#1116), and remaining Tier 1H CLI
  tests (#1138) to subscription-based or pause-based timing patterns;
  remaining wall-clock timing assertions removed (#1088/#1097, #1109).

- **`TimeProvider` for daemon mtime in tests (#1083).** Daemon-side
  mtime checks now consume an injected `TimeProvider` so tests can pin
  observed time without sleeping.

- **Connector parity matrix (#1052).** Asserts feature parity across the
  GitHub / Linear / Notion / Slack connectors so divergence is caught at
  test time.

- **Split large dispatcher and orchestrator HTTP tests (#1053).** Long
  monolithic test files broken into focused sub-suites for faster
  individual reruns and clearer failure attribution.

- **Remove deprecated polling test helpers (#1122).** Cleans up
  `assert_poll_*`-style helpers superseded by subscription-based
  patterns.

- **Subprocess-CLI test conversion (#1116/#1133).** Converts CLI tests
  that previously spawned the `harn` binary to in-process invocation;
  binary-surface tests are gated as `#[ignore]` and run in the slow E2E
  job.

## v0.7.52

### Added

- **Harn-native Merge Captain persona (#1009/#1029).** Replaces the
  shell-driven Merge Captain MVP with a deterministic Harn package owning
  multi-repo PR queue scheduling, durable per-PR state, audit-grade merge
  receipts, and a 12-state machine over each tracked PR with legal-edge
  enforcement.

- **Merge Captain JSONL transcript oracle (#1013/#1030).** New
  `harn merge-captain audit <transcript>` CLI plus oracle infrastructure
  for inspecting JSONL artifacts (extra model calls, invalid structured
  outputs, repeated reads, missing approvals, non-minimal tool usage).
  Ships five reference goldens.

- **`llm_stream_call` streaming builtin (#1038).** Native LLM streaming
  builtin returning `Stream<...>` with cancellation support; provider
  capability matrix grows a `streaming` column.

- **`schema_recover` stdlib helper (#1031).** Malformed-JSON repair helper
  for tolerating common LLM-output mistakes.

- **Generic webhook intake substrate (#1011/#1037).** Runtime + stdlib
  primitives for receiving and routing webhooks.

- **HITL primitives promoted to typed syntax (#926/#1034).** `ask_user`,
  `dual_control`, `escalate_to`, `request_approval` are now first-class
  keywords with typed AST and conformance coverage.

- **Intersection types `A & B` (#914/#1033).** New structural composition
  type with parser/typechecker/formatter/tree-sitter coverage; nested
  unions parenthesise in the formatter for round-trip clarity.

- **Match expressions in typed value positions (#1043).** Match arms now
  unify type-inferentially in expression positions, enabling
  `let result = match x { ... }`.

- **Improve type mismatch diagnostics (#1045).** Richer typechecker
  diagnostics with reduced redundant nested-mismatch walks.

- **Auto-size formatter section headers (#1040).** Formatter now sizes
  section headers to the longest line in each block.

- **Trailing comma formatting policy (#1044).** Formatter enforces a
  consistent trailing comma policy across multi-line collections.

- **Agents API SDK codegen gate (#1041).** Surface verification gate for
  `harn-cloud` Agents API SDK generation.

- **Typed orchestrator + package CLI errors (#945/#1032).** Migrates
  orchestrator and package CLI modules off `Result<T, String>` to typed
  error enums with structured variants.

- **Atomic on-disk state writes (#1028).** Standardizes temp-file +
  rename pattern via `harn_vm::atomic_io::atomic_write` across lockfile,
  manifest, registry, and cache writers.

- **Virtual-clock test migration (#948/#1027).** Timing-sensitive tests
  migrated to a deterministic virtual clock to reduce flakes.

- **`cfg(unix)` test gate audit + nightly Windows matrix (#1026).**
  Surfaces and trims tests gated on `cfg(unix)` and adds a nightly
  Windows nextest matrix to keep cross-platform coverage honest.

- **macOS ad-hoc codesign build target (#1042).** New
  `make build`/`make build-release` targets that ad-hoc sign the harn
  binaries on macOS for local development convenience.

### Fixed

- **Protocol retry spiral after recovered tool calls (#1039).** Tool-call
  recovery no longer triggers an infinite protocol-retry loop.

- **dyld pre-warm + nextest cap 4 → 8 (#1035).** Pre-warms dyld in
  harn-cli subprocess tests and raises the nextest concurrency cap to
  cut local + CI test wall-clock.

- **Windows-only typed error returns** (fix-forward). The #1032
  migration left two `cfg(not(unix))`/`cfg(windows)` paths returning
  `Result<(), String>` against typed signatures (Ctrl-C loop in
  `serve.rs`, `symlink_path_dependency` in `registry.rs`); both now
  return their typed `OrchestratorError` / `PackageError::Registry`
  variants.

## v0.7.51

### Added

- **`cost_route` language block (#967/#1025).** New `cost_route` declaration
  that resolves provider/model selection from declarative cost/latency/quality
  budgets, with options validated at parse time. Lowers to a runtime cost
  router with conformance coverage and provider/options helper updates across
  Anthropic, OpenAI-compatible, Gemini, Ollama, Bedrock, Azure OpenAI, and
  Vertex shims.

- **Streaming `parallel each` and event-log subscriptions (#970/#1023).**
  New `parallel each as stream` mode lazily yields per-element results as
  `Stream<T>`, plus a `parallel race` builtin that returns the first
  successful settle. Adds `event_log_subscribe` for tailing the in-process
  event log as a typed stream. Parser/AST/typechecker/formatter, VM
  `ParallelMapStream` op, runtime stdlib, conformance, and spec coverage.

- **First-class `eval_pack` declarations (#965/#1022).** New `eval_pack`
  syntax that lowers to normalized eval-pack manifests, with optional
  block/summarize execution in script and block position. Exposes
  `eval_pack_manifest` / `eval_pack_run` builtins and manifest-level
  default baselines. Formatter, linter, LSP, tree-sitter, docs, generated
  keywords, and conformance coverage updated; per-case `compare_to`
  overrides manifest-level `baseline`.

- **Provider-capability LLM option gating (#869/#1024).** `llm_call` options
  are now gated against the provider capability matrix with consistent
  provider/model error messages and a `harn check --provider-matrix` hint.
  PDF capability metadata flows through provider rows, `model-info`,
  `provider_capabilities`, docs, and matrix filters. CLI replay / mock-replay
  paths bypass gates to keep fixture-backed tests unblocked.

- **Signed provenance receipts (#931/#1018).** Adds Merkle-chained EventLog
  provenance headers with per-topic prev/record hashes and Ed25519-signed
  receipts. New `harn run --attest` / `--receipt-out` / `--attest-agent`
  flags plus a top-level `harn verify` command for receipt verification with
  tamper detection. Parser/unit coverage and CLI docs updated.

- **Guarded stdlib tool synthesis (#966/#1017).** New `tool_synthesize`
  builtin produces deterministic, cached callable tools from natural-language
  specs. Default executor stays in dry-run; explicit `host_bridge` and
  `mcp_server` dispatch paths are gated by existing `host_tool_call` /
  `mcp_call` policy checks. Conformance coverage and highlight metadata
  refreshed.

## v0.7.50

### Added

- **`schema_recover` malformed-JSON repair helper (#906).** New stdlib
  `schema_recover(text, schema, opts?)` runs a three-tier deterministic
  recovery cascade — direct JSON parse → extract from prose / code
  fences → regex scrape of top-level `key: value` lines — followed by
  an optional one-shot `llm_call` repair pass. Returns the same
  `{ok, data, raw_text, error, error_category, attempts, stage,
  repaired}` envelope shape as `llm_call_structured_result`, with
  `stage` reporting which tier succeeded. Designed to replace
  hand-rolled `normalize_*()` chains downstream of LLM calls; set
  `{llm_repair: false}` for a fully deterministic recovery pass.
- **Generic webhook intake substrate (#1011).** Forge-agnostic intake layer
  any connector can wire into. Connectors declare a path scope, signature
  header + algorithm (`sha256` or legacy `sha1`), delivery-id header, and
  topic; the substrate handles HMAC verification, durable delivery-id
  deduplication via the trigger inbox, and republishes accepted deliveries
  onto the connector's chosen topic. Exposed as `webhook_intake_register`,
  `webhook_intake_feed`, `webhook_intake_recent`, `webhook_intake_list`,
  and `webhook_intake_deregister` builtins. Per-forge event normalization
  remains in each connector repo. Includes `hmac_sha1` builtin for legacy
  signature schemes. New `triggers/webhook-intake.md` doc and conformance
  coverage for happy path, signature rejection, replay dedupe, and
  multi-path isolation.
- **Intersection types `A & B` (#914).** New type-expression form for
  composing structural shapes — `fn use(ctx: BaseCtx & AuthCtx)` accepts
  values that satisfy every component, and the typechecker resolves field
  access against any participating shape. `&` binds tighter than `|`
  (`A & B | C` parses as `(A & B) | C`). At runtime the annotation lowers
  to a JSON-Schema `allOf` guard, so the parameter-runtime check rejects
  values missing fields from any component. AST, lexer (`Amp` token),
  parser, subtyper, formatter, linter, LSP semantic tokens, VM schema
  lowering, tree-sitter grammar/tests, spec, quickref, and conformance
  coverage updated.

- **Enterprise LLM providers (#870/#976).** First-class shims for Bedrock
  Runtime (Converse API with hand-rolled AWS SigV4 signing and
  env/profile/container/instance credential resolution), Azure OpenAI
  (deployment-name routing with API-key and Entra bearer-token auth), and
  Vertex AI Gemini (`generateContent` routing with bearer-token and
  service-account JWT exchange). Provider registry, default config, inference
  rules, capability matrix, and conformance coverage updated; existing
  `provider_overrides` apply uniformly to the new providers.

- **Provider capability matrix (#871/#979).** New `harn check
  --provider-matrix` with text/JSON output and feature filtering, backed by
  vision/audio/JSON-schema fields on the live capability schema. Generated
  `docs/src/provider-matrix.md` is gated by a CI drift check.

- **Multimodal image content (#863/#977).** Provider-neutral image content
  blocks for `llm_call` with base64/URL validation and implicit `vision`
  capability gating. Translation at provider boundaries for Anthropic,
  OpenAI-compatible, Gemini (now natively registered), and Ollama, plus
  `vision_supported` capability and conformance coverage.

- **First-class stream generators (#963/#978).** Contextual `gen fn`
  declarations and `emit` statements with `Stream<T>` runtime support
  alongside existing `yield`/`Generator<T>`. Parser/AST/formatter/linter/LSP,
  tree-sitter grammar, docs, and spec coverage. Streams support `for`,
  `.next()`, `.iter()`, nesting, early break, and body-error propagation;
  `gen` stays contextual so existing identifiers keep parsing.

- **LLM prompt cache usage (#862/#980).** New `usage` dict on `llm_call`
  results with cache read/write tokens, Anthropic
  `cache_creation_input_tokens`, `cache_hit_ratio`, and
  `cache_savings_usd`. Parsed across Anthropic and OpenAI-compatible response
  shapes and surfaced through structured-result envelopes; mock provider
  simulates cache warm/read behavior.

- **Unified LLM thinking config (#849/#971).** Typed `ThinkingConfig`
  boundary with disabled, enabled-budget, adaptive, and effort modes. The
  capability matrix replaces the `thinking` boolean with `thinking_modes`
  (legacy boolean preserved for queries). Mapped through Anthropic,
  OpenAI-compatible/OpenRouter, and Ollama providers, including OpenAI
  `reasoning_effort`.

- **LLM budget envelopes (#868/#961).** First-class `budget` envelopes on
  `llm_call` with pre-flight `max_cost_usd`, `max_input_tokens`,
  `max_output_tokens`, and `total_budget_usd` checks. Structured terminal
  `budget_exceeded` errors flow through `try { ... }`. Same envelope threads
  through `agent_loop`, workflow stages, and `sub_agent_run`, with aggregate
  loop `total_budget_usd` stopping before an over-budget turn starts.

- **Agent loop profiles (#861/#960).** New `tool_using`, `researcher`,
  `verifier`, and `completer` profiles with sensible defaults and explicit
  per-call overrides. Profile parsing applies across direct, bridge-backed,
  `sub_agent_run`, and workflow agent-loop config paths. Adds
  `agent_loop` schema-retry handling so verifier/schema profiles retry
  invalid structured output before accepting a turn.

- **`agent_loop` MCP server tools (#860/#962).** `agent_loop({ mcp_servers:
  [...] })` bootstraps configured HTTP/stdio MCP servers, lists tools once,
  prefixes them as `<server>__<tool>`, and merges them into the tool
  registry/native tool surface. Prefixed calls route back through the live
  client using the unprefixed MCP name; lifecycle cleanup, docs, and
  conformance with a mocked HTTP MCP server included.

- **MCP Streamable HTTP transport (#872/#972).** Canonical Streamable HTTP
  GET/DELETE on the `/mcp` endpoint with `Mcp-Session-Id` continuity, plus
  event-stream-only POST responses negotiated as SSE `message` events while
  JSON clients keep `application/json`. Legacy SSE `/sse` and `/messages`
  routes carry `Deprecation: true`; protocol/origin validation hardened.

- **Project prompt templates over MCP (#879/#974).** `harn mcp serve` now
  exposes project and prompt-library `.harn.prompt` files via `prompts/list`
  and `prompts/get`, rendering with supplied arguments, validating required
  args, and returning text plus structured image content. Advertises
  `prompts.listChanged` and emits `notifications/prompts/list_changed` on
  prompt/manifest hot reloads for stdio and legacy SSE clients.

- **Merge Captain transcript oracle and audit CLI (#1013).** New
  `harn merge-captain audit <transcript>` consumes JSONL transcript
  artifacts from CLI/TUI/IDE/hosted runs and reports oracle findings:
  extra model calls, invalid structured outputs, repeated reads, bad
  waits, unsafe attempted actions, skipped verification, missing
  approvals, and non-minimal tool usage. Supports JSON-formatted
  golden fixtures with declarable state-machine steps, tool-pattern
  approvals/forbidden actions, and per-scenario budgets; ships five
  reference goldens (green PR, failing CI, semantic conflict, merge
  queue, new-PR arrival) plus a negative `bad_unsafe_merge` fixture
  under `examples/personas/merge_captain/`. Outputs both a
  human-readable report and machine-readable JSON suitable for CI
  gates, with each finding linked to transcript event ids and PR
  state-machine steps.

### Changed

- **Canonical LLM error taxonomy enforced in retry (#854/#958).** Retry
  decisions now consult the structured `kind` (`transient`/`terminal`) and
  `reason` first; OpenAI-compatible, Anthropic, and Ollama HTTP/provider
  shapes are mapped (context overflow, content policy, auth, invalid
  request, model unavailable, rate limit, timeout, network, server). Legacy
  `category` field and `[rate_limited]` / `[http_error]` message tags
  preserved for compatibility.

- **Audit hardening across runtime surfaces (#848).** Auth, egress, portal
  asset serving, skill substitution, tenant registry writes, and namespaced
  skill lookup tightened. Schema validation composition fixed so
  `all_of`/union still apply sibling constraints. Host command handling now
  uses canonical cwd, UTF-8-safe inline output, Gradle wrapper portability,
  and a split artifact module. Tests isolate global egress policies and
  agent event sinks to remove parallel-suite contamination.

- **Module splits for maintainability.** `crates/harn-cli/src/package.rs`
  split into focused submodules (`manifest`, `validation`, `registry`,
  `lockfile`, `extensions`, `package_ops`, `skills`, with shared
  `test_support`); every new file under 2,000 LOC (#938/#975). The trigger
  dispatcher gains `predicate_eval.rs` (predicate evaluation, cache replay,
  budget accounting, action-graph metadata) and `circuits.rs` (destination
  circuit state/probing, budget/circuit DLQ helpers), reducing
  `triggers/dispatcher/mod.rs` from 4633 to 3827 LOC with no behavior change
  (#940/#973).

### Tests

- **Subprocess test harness pre-warm (#949).** Every harn-cli integration
  test that spawns the `harn` debug binary now goes through
  `test_util::process::harn_command()`, which performs a one-shot
  `harn --version` synchronously on first call within each test-binary
  process. That single invocation page-ins the binary's `__TEXT` segment
  and shared libraries into the macOS unified buffer cache and drives
  AMFI signature validation once, so subsequent parallel cohorts no
  longer contend on dyld/AMFI cold-cache resources. With the
  architectural fix in place, the `harn-subprocess` and `harn-cli-bin`
  nextest test groups in `.config/nextest.toml` relax `max-threads` from
  4 to 8 (PR #837 documented the previous cap as a workaround). New
  `scripts/stress_subprocess_tests.sh` reproducibly stress-tests the
  suite at the new cap.

## v0.7.49

### Added

- **Native `llm_call` transport retries (#853).** Raw `llm_call` and
  structured wrappers now accept `llm_retries` and `llm_backoff_ms` for
  transient provider failures, sharing the canonical LLM error taxonomy while
  keeping schema retries independent. Plain `llm_call` remains fail-fast by
  default (`llm_retries: 0`, `llm_backoff_ms: 250`).

- **Canonical LLM error taxonomy (#854).** Provider transport errors now surface
  `kind` (`"transient"` / `"terminal"`) and `reason` (`"rate_limit"`,
  `"server_error"`, `"network_error"`, `"timeout"`, `"auth_failure"`,
  `"context_overflow"`, `"content_policy"`, `"invalid_request"`,
  `"model_unavailable"`, `"unknown"`) alongside the existing `category`
  compatibility field. `llm_call` / `agent_loop` retry only transient kinds,
  and provider-specific HTTP mappings cover OpenAI-compatible, Anthropic, and
  Ollama error shapes.

- **Command policy hooks (#824/#846).** First-class `command_policy` builtins
  with policy stack management, deterministic command/result risk scanning,
  and a safe `command_llm_risk_scan` fallback shape. `host_call("process.exec",
  …)` is now wrapped with preflight/postflight policy handling supporting
  `deny`, `require_approval`, `dry_run`, `explain_only`, constrained rewrites,
  audit attachment, feedback, and hook-recursion protection. Command policies
  thread through `agent_loop`, `sub_agent_run`, and workflow model-policy
  scoping. Conformance coverage, spec/builtin/highlight docs included.

- **Shell discovery host capability (#826/#845).** New shared shell discovery
  contract — `process.list_shells`, `process.get_default_shell`,
  `process.set_default_shell`, `process.shell_invocation` — backed by a JSON
  schema at `spec/schemas/host-shell-discovery.schema.json`. VM shell builtins,
  `process.exec` shell mode, hostlib `run_command` shell mode, ACP
  `terminal/create`, and stdlib wrappers now route through selected-shell
  metadata instead of hardcoded `sh`/`cmd` assumptions. Conformance and docs
  included.

- **Harn-owned plan artifacts (#816/#844).** New `import "std/plan"` module with
  `harn.plan.v1` normalization and VM-owned `emit_plan` / `update_plan` local
  tool handling. Structured plan transcript events are persisted and surfaced as
  `AgentEvent::Plan`. Plan artifacts propagate through ACP (`harnPlan`) and A2A
  (`harn_plan`) while the standard ACP `entries` projection is preserved.

- **Command runner v2 (#823/#842).** Canonical command runner envelope for
  `run_command`, background command execution, and `process.exec` with
  argv/shell modes, command IDs, PID/process-group metadata, timing,
  exit/signal/timeout status, capture counts, artifact paths, hashes, and
  sandbox/audit metadata. New `read_command_output` builtin reads persisted
  command artifacts by command ID, handle, or path with bounded reads and path
  validation.

- **Agent tool surface validation (#825/#843).** VM-level validation of tool
  surfaces for registries/native tools, capability policies, approval policies,
  prompt references, deferred tools, side-effect ceilings, and execute artifact
  result readers. Exposed as `tool_surface_validate(…)` builtin; `harn check`
  preflight validates literal prompt/tool surfaces statically. Diagnostic codes
  and artifact-reader annotations documented.

- **`harn connector test` package gate (#812/#841).** Single first-class command
  that gates connector packages through metadata validation, `harn
  check`/`lint`/`fmt --check`, contract fixtures, package-local fixture
  programs, clean consumer install/import smoke, and standalone doc example
  parsing. Connector docs, trigger quickref, and connector template CI guidance
  updated.

- **Protocol conformance gate (#807/#840).** New `harn test protocols` command
  runs offline JSON schema fixture conformance for ACP, A2A, and MCP. Checked-in
  protocol schemas (`mcp-2025-11-25`, `acp-session-update`, `a2a-0.3.0`) and
  positive/negative fixtures live under `conformance/protocols/`. Wired into
  `make all`, CI, and the release gate.

- **GraphQL connector SDK substrate (#811/#834).** `import "std/graphql"` adds
  provider-neutral helpers for request bodies, auth headers, normalized
  `{data, errors, extensions, meta}` response envelopes, rate-limit metadata,
  cursor pagination, SDL/introspection normalization, and generated-style
  operation wrappers. The Linear stdlib connector now drives outbound
  issue/comment/search helpers through shared GraphQL operation specs. Module
  mirrored to root `stdlib/graphql.harn`. Conformance and Linear fixture schema
  included.

### Changed

- **A2A 0.3.0 RPC names (#886).** `harn serve a2a` now advertises protocol
  version `0.3.0`, accepts canonical A2A methods such as `message/send`,
  `message/stream`, `tasks/get`, `tasks/cancel`, `tasks/resubscribe`, push
  notification config methods, and `agent/getAuthenticatedExtendedCard`, and
  emits `Deprecation: true` for the legacy `a2a.*`, `tasks/send`,
  `tasks/send_and_wait`, and `tasks/sendSubscribe` aliases. Outbound
  `a2a://...` trigger dispatch now sends canonical `message/send`.

- **MCP latest-spec gap handling documented and enforced (#809/#839).** Shared
  helper explicitly rejects unsupported MCP 2025-11-25 methods
  (`completion/*`, `resources/subscribe`, `roots/*`, `sampling/*`,
  `elicitation/*`, `tasks/*`) with typed JSON-RPC unsupported-feature errors on
  both client and server paths. MCP client/server support matrices documented
  in the MCP guides.

- **`self_review` verifier rounds improved (#838).** Schema retry nudges now
  include bounded nested object/array shape, not just top-level keys. Later
  `self_review` rounds act as verifier/adjudication passes that keep only
  diff-supported candidate findings, dropping speculative or verify-only issues.
  Output instructions tightened to allow only explicit semantic aliases.

- **Hostlib docs decoupled from Burin migration language (#830).** `harn-hostlib`
  README and host-contracts migration doc rewritten in Harn-generic terms,
  removing references to Burin-specific implementation waves and burin-code
  internal paths.

### Fixed

- **`orchestrator_http` and connector test flake eliminated (#837).** Flaky HTTP
  orchestrator and connector tests refactored architecturally: server lifecycle
  management and ephemeral port handling made deterministic so the tests no
  longer race under `cargo nextest`.

- **HTTP mock call records expose response headers (#829).** HTTP mock call
  records now include the `response_headers` field so scripts can assert on or
  inspect headers returned from mock endpoints.

- **Release bump PRs auto-merge on CI pass (#835).** Recovery version-bump PRs
  created by `release_ship.sh` now enable squash auto-merge immediately after
  creation, removing the manual merge step after CI clears.

## v0.7.48

### Added

- **MCP script surfaces served over HTTP (#808/#828).** Script-authored
  `mcp_tools` / resources / templates / prompts now serve over Streamable
  HTTP as well as stdio, sharing the VM MCP JSON-RPC handler with the
  new script HTTP wrapper (sessions, Streamable HTTP POST/GET, legacy
  SSE compatibility). DispatchCore `pub fn` MCP servers can now carry
  Server Card metadata, and the surface adds resource/list/read plus
  prompt/resource-template discovery fallbacks with cursor pagination.

- **A2A AgentCard discovery alignment (#806/#821).** Serve
  `/.well-known/agent-card.json` as the canonical A2A discovery endpoint
  while preserving legacy aliases (`/agent/card`, `/.well-known/a2a-agent`,
  `/.well-known/agent.json`). The emitted card now matches the current
  upstream schema — `supportedInterfaces` array (with `protocolBinding` /
  `protocolVersion`), `defaultInputModes` / `defaultOutputModes`,
  per-skill `tags` / `examples` / `inputModes` / `outputModes`,
  `securitySchemes` as object — and outbound A2A discovery prefers the
  canonical path with a fallback chain. The Harn Agents API card schema
  is now `HarnAgentCard` with a nested `a2a_agent_card` projection.

- **ACP lifecycle extension contract formalized (#817/#820).** Harn now
  advertises its ACP extensions during `initialize` under
  `agentCapabilities._meta.harn`, including pinned upstream-schema
  compatibility, extension `sessionUpdate` discriminators, and the
  tool/content extension fields. Emitted `plan` updates align with the
  upstream schema by using `entries` instead of the old Harn-local
  `plan` key. Host rendering and compatibility expectations are
  documented in `docs/src/bridge-protocol.md`.

### Changed

- **OpenAI-compatible streaming preserves token usage (#173/#818).**
  OpenAI-compatible streaming endpoints (including Ollama when wired
  through `/v1/chat/completions`) now request
  `stream_options.include_usage` so usage figures survive streamed
  responses. Native Ollama `/api/chat` behavior is preserved (usage
  arrives in the final NDJSON chunk). OpenRouter / OpenAI-compatible
  cache-write usage is now read from
  `usage.prompt_tokens_details.cache_write_tokens`.

- **Documentation IA reorganized (#814/#819).** mdBook summary split
  into smaller top-level groups (language, agent runtime, protocols,
  orchestration, packages/connectors, observability, operations,
  reference, migrations). New protocol support matrix and cross-links
  to the canonical MCP/ACP/A2A guides. Release-workflow material moved
  out of the CLI reference, README release section trimmed, and stale
  implementation-wave issue/path references removed from user docs.

### Fixed

- **`hostlib_*` calls in static `.harn` files no longer break
  `harn check --workspace` and `harn lint`.** Hostlib builtins
  (`hostlib_code_index_*`, `hostlib_ast_*`, `hostlib_scanner_*`, …)
  are registered onto the VM at runtime by
  `harn_hostlib::install_default` and have no static signature in the
  parser's BUILTIN_SIGNATURES table. Before this fix, calling them
  directly from a workspace `.harn` file raised
  `error: call target X is not defined or imported` at typecheck and
  `lint[undefined-function]` warnings during lint. The cross-module
  resolver and the lint's call-graph walk now treat `hostlib_*` as an
  opaque escape hatch — the same way `__`-prefixed names are
  treated. The runtime VM still does the real dispatch, so typos
  surface there instead of at parse time (the same trade-off
  `host_call("…")` already accepts).

## v0.7.47

### Added

- **GraphQL connector SDK substrate (#811).** Adds `import "std/graphql"`
  for provider-neutral GraphQL-over-HTTP request bodies, auth headers,
  normalized `{data, errors, extensions, meta}` envelopes, persisted-query
  metadata, cursor pagination helpers, SDL/introspection normalization, and
  generated-style operation wrapper source. The Linear stdlib wrapper now
  drives outbound issue/comment/search helpers through shared GraphQL
  operation specs, with a fixture schema and conformance smoke test covering
  the generated-client path.

- **Long-running tool handles for `run_command`, `run_test`,
  `run_build_command` (#778/#803).** Pass `long_running: true` to spawn
  the child process without blocking and receive a handle dict immediately
  — `{ handle_id, started_at, command }`. A background waiter drains
  stdout/stderr on separate threads (preventing pipe deadlock), calls
  `child.wait()`, and injects the result into the agent's next turn via a
  new cross-thread feedback queue (`push_pending_feedback_global` /
  `drain_pending_feedback`). A new `tools.cancel_handle` builtin
  SIGKILLs an in-flight handle by ID and suppresses the pending feedback
  push. Session-end cleanup fires automatically: orphaned child processes
  are killed when the agent-loop session exits via
  `register_session_end_hook` / `cancel_session_handles`.

- **AST hostlib symbol mutation + bracket balance (#775/#798).** Four
  new builtins on the `ast::*` hostlib surface so burin-code can retire
  `SymbolOperations.swift` and `BracketBalance.swift`:
  `ast.symbol_extract` locates a named symbol and returns its text with
  1-based inclusive line range; `ast.symbol_delete` removes it and
  collapses resulting blank-line runs; `ast.symbol_replace` splices
  caller-provided text in place of the symbol's preamble + signature +
  body (both mutating ops re-parse to validate); `ast.bracket_balance`
  returns signed paren/bracket/brace counts using a string + comment
  lexer. All four follow the standard tagged-union envelope with result
  variants `extracted` / `removed` / `replaced` / `not_found` /
  `ambiguous` / `unsupported_language` / `syntax_error_after_edit`.

- **`parse_junit_xml` stdlib builtin (#801).** New core builtin that
  parses a JUnit XML test report (`string` or `bytes`) into a list of
  per-case dicts with `name`, `status` (`"passed"` / `"failed"` /
  `"skipped"` / `"errored"`), `duration_ms`, `message`, `stdout`, and
  `stderr` keys. JUnit XML is the de facto interchange format for
  GTest (`--gtest_output=xml`), Maven Surefire / Gradle, xUnit,
  pytest, vitest, and cargo-nextest's JUnit dialect, so a single
  builtin lets a Harn script wrapping a `process.run` of a compiled
  test runner extract structured pass/fail data without going through
  a host capability. The parser is intentionally lenient: malformed
  input yields `[]` instead of throwing.

## v0.7.46

### Added

- **AST hostlib: function-body and import extraction (#774).** Three
  new builtins land on `crates/harn-hostlib/src/ast/`:
  `hostlib_ast_function_body` extracts a single named function's body
  (with an optional containing class/struct filter),
  `hostlib_ast_function_bodies` is the bulk variant returning a map
  keyed by name, and `hostlib_ast_extract_imports` returns the
  document-ordered list of import declarations for a source file.
  Each accepts either an in-memory `source` string (with `language`)
  or a `path` (with optional `language` override). The response
  shapes mirror Swift's `FunctionBodyExtractor.ExtractedBody` and
  `TreeSitterImportExtractor` byte-for-byte — line coordinates inside
  function bodies are 1-based to match Swift, while symbols/outline
  stay 0-based — so burin-code's existing decoders can route through
  `HarnASTHostlibClient` unchanged. Full tree-sitter coverage: every
  one of the 21 supported grammars feeds the same walker, including
  the JS/TS arrow-function and Elixir `def`/`defp` matchers, plus a
  fallback keyword scan for `extract_imports` when the grammar's AST
  didn't surface anything. Unblocks burin-code's
  [#289](https://github.com/burin-labs/burin-code/issues/289) — the
  ASTEngine call sites in `ASTRPC`, `HarnHostServer+ASTPrimitives`,
  and `APIContractExtractor+Extraction` can now retire in favor of a
  single Harn round-trip. JSON schemas ship under
  `crates/harn-hostlib/schemas/ast/` for the cross-repo schema-drift
  test in burin-code's `HarnASTHostlibContractTests`.

### Changed

- **Sharper diagnostics for missing `render_prompt` / `render` targets
  (#771).** `harn check`'s preflight pass already validated literal
  template paths, but the message read `preflight: render target ...`
  regardless of which builtin was called and the help text only ever
  pointed at the generic guidance. The diagnostic now names the actual
  builtin (`render_prompt target ...` vs `render target ...`) and, when
  the missing basename has a unique match elsewhere under the project
  root, prepends a `did you mean '<rel>'? (found at <abs>)` suggestion
  so the most common typo — file misfiled in a sibling directory —
  becomes a one-keystroke fix. Raw string literals (`r"..."`) are also
  validated alongside ordinary string literals; non-literal first
  arguments (variables, interpolated strings) continue to be skipped
  silently.

### Performance

- **Cache schema `pattern` regexes.** `schema.is`, `schema.expect`, and
  `schema.result_of` previously called `regex::Regex::new` once per
  validated value when the schema declared a `pattern`. Compilation
  now happens at most once per pattern (capped 256-entry cache,
  cleared when full), eliminating wasted work for hot-path validators
  that re-validate the same shape thousands of times.
  Crate: `harn-vm` (`schema/validate.rs`).

### Fixed

- **`harn-serve` ACP loader no longer silently swallows source-read
  failures.** When the ACP bridge could not read the requested
  `.harn` source for diagnostic context, it previously fell back to
  empty source and continued, which produced misleading runtime
  errors with no surrounding code. The error is now surfaced as
  `failed to read pipeline source '<path>' for diagnostic context:
  …`. Crate: `harn-serve`.
- **`harn-lint` legacy-doc-comment scan no longer does O(n²) `Vec`
  shifts** when trimming blank lines off a recovered `///` block.
  Crate: `harn-lint` (`harndoc.rs`).

### Internal / DX

- **Centralized CLI timestamp/duration formatting in
  `harn_cli::format`.** Five copies of `format_timestamp`,
  `format_duration`, and `format_ms` across `trust`, `trigger
  replay`, `portal/dlq`, `portal/util`, and `orchestrator` commands
  are gone — the `orchestrator` and `portal` callsites now thinly
  re-export from the shared module so user-facing output stays
  byte-identical.
- **`harn fmt`'s internal API uses `FmtMode::{Check, Write}`** instead
  of a boolean flag, so call sites read what they mean rather than
  `fmt_targets(targets, true, ...)`.
- **CLI help text clarifications** for `harn explain`, `harn doctor`,
  `harn publish`, and `harn trust-graph`. The previously cryptic
  `harn explain` description now names the invariants it walks
  (`fs.writes`, `approval.reachability`, …); `harn publish` is honest
  about the registry-submission gap and points users at
  `harn add <repo-or-path>` in the meantime.
- **Spec preamble now identifies as pre-1.0** (tracking the workspace
  `0.7.x` series) instead of claiming "Version: 1.0", which was
  misleading given the surface-level breaking changes that still ship
  across minor versions.
- **Removed stale `#[allow(dead_code)]` annotations** on
  `Manifest::package` and `Manifest::exports`; both fields are read
  by package-loading and doctor code paths.

### Tests

- Added regression coverage for the new schema `pattern` cache:
  repeated validation against a valid pattern still succeeds, and an
  unparsable pattern (e.g. `[unclosed`) returns a stable
  `invalid regex pattern` error rather than panicking on the cached
  re-fetch.

## v0.7.45

### Added

- **`hostlib_ast_parse_errors` and `hostlib_ast_undefined_names`
  (#773).** Two new builtins on the `ast::*` hostlib surface so
  burin-code's syntax-validation and tertiary-diagnostic paths can
  delete their Swift `Sources/ASTEngine/TreeSitterParseErrors.swift` /
  `TreeSitterUndefinedNames.swift` fallbacks. `parse_errors` walks the
  tree-sitter parse for `ERROR` and `MISSING` nodes and emits a flat
  list with 0-based row/column ranges, byte ranges, a short
  human-readable `message`, the offending `snippet` (truncated to 60
  chars, newlines escaped), and a `missing` flag — plus
  `top_level_decl_count` keyed off the same per-language declaration
  table the Swift fallback uses (TypeScript, JavaScript, Go, Rust,
  Python, Java, C/C++, C#, Kotlin, Ruby, PHP, Scala). Both `content`
  and `path` payloads are accepted; `language` accepts canonical wire
  names ("python", "typescript") or bare extensions ("py", "ts") for
  parity with the Swift call sites. `undefined_names` ports the
  per-language profile walker for Python, JavaScript (incl. JSX),
  TypeScript (incl. TSX), Go, and Ruby, dedupes references by name,
  and filters against the curated builtin stop-list. Profiles for
  unsupported languages return `supported = false` so callers fall
  back to an external linter (LSP / ruff / tsc / go vet). New JSON
  schemas under `crates/harn-hostlib/schemas/ast/parse_errors.*.json`
  and `undefined_names.*.json` ship with the crate so downstream
  schema-drift tests see the contract.
- **`code_index` hostlib gains live workspace state (#776).** The
  `code_index` capability now ships the agent registry, advisory file
  locks, append-only version log, file-id assignment surface, and
  cached read paths that previously lived in burin-code's Swift
  `Sources/BurinCodeIndex/`. Twenty-two new builtins —
  `agent_register/heartbeat/unregister`, `current_agent_id`, `status`,
  `lock_try/release`, `current_seq`, `changes_since`, `version_record`,
  `path_to_id`, `id_to_path`, `file_ids`, `file_meta`, `file_hash`,
  `read_range`, `reindex_file`, `trigram_query`, `extract_trigrams`,
  `word_get`, `deps_get`, and `outline_get` — sit alongside the
  existing five. Each is JSON-Schema-locked under
  `crates/harn-hostlib/schemas/code_index/`, and `CodeIndexCapability`
  now exposes `restore_from_disk`/`persist_to_disk` for daemon
  recovery (snapshot lives at `<root>/.burin/index/snapshot.json`)
  plus a `set_current_agent` slot for embedders to bind per-call agent
  identity. Concurrency is exercised end-to-end by
  `tests/code_index_live_state.rs`, which fans out 8 native threads
  through register/heartbeat/lock/release/unregister cycles and asserts
  `version_record` assigns globally unique seq numbers. Closes #776 and
  unblocks burin-code#296 (deletion of `Sources/BurinCodeIndex/`).
- **Cross-slice predicate budget scheduler (#736).** `PredicateExecutor`
  now schedules predicate work fairly across multiple candidate slices
  via a new `execute_slices(slices)` entrypoint and the
  `PredicateSchedulerConfig` knobs. Global semaphores cap concurrent
  deterministic and semantic predicate evaluations across all slices;
  per-slice caps (default semantic-per-slice = 1) keep one slice from
  monopolizing scarce semantic lanes. Each slice independently tracks
  aggregate per-kind wall-clock against `slice_deterministic_envelope`
  and `slice_semantic_envelope`. When a slice's envelope exhausts,
  every remaining predicate of that kind for that slice short-circuits
  to a structured `Block { error: { code: "budget_exceeded" } }` —
  never a panic, never an implicit approval — while other queued
  slices continue unaffected. Output ordering remains deterministic:
  records sort by predicate hash within each slice, and reports stay
  in input slice order. Closes #736 and lands the implementation
  follow-up to the design in `docs/src/flow-predicates.md`.
- **`meta-invariants.harn` bootstrap policy validation (#734).** Closes
  decision 2 of the predicate-language design record. A repo-root
  `meta-invariants.harn` file (separate from per-directory
  `invariants.harn`) carries a hand-authored bootstrap policy: its
  `sha256:` content hash and a maintainer list pulled from a
  `@bootstrap_maintainers(approvers: [...])` attribute (defaulting to
  `role:flow-platform` when absent). Two new library entrypoints in
  `crates/harn-vm/src/flow/predicates/bootstrap.rs` enforce the policy:
  `validate_predicate_edit` promotes the parser's soft warnings on
  proposed `invariants.harn` edits into hard `Block` verdicts with stable
  codes (`bootstrap_missing_archivist`,
  `bootstrap_archivist_provenance_incomplete`,
  `bootstrap_kind_collision`, `bootstrap_missing_semantic_fallback`),
  and `validate_bootstrap_edit` rejects Archivist authorship of
  `meta-invariants.harn` outright (`bootstrap_archivist_cannot_author_bootstrap`)
  while routing every other author to `RequireApproval` against a
  maintainer from the previous policy. Both validators pin the previous
  policy hash so the slice approval chain has an explicit audit pointer.
  `harn flow ship watch` and `harn flow archivist scan` surface a new
  `bootstrap_policy` field in their JSON payloads carrying the discovered
  hash, maintainer list, and parser diagnostics — or `status = "absent"`
  when the file is missing.

## v0.7.44

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

### Changed (breaking)

- **Explicit tool executors and pre-flight tool-registry validation
  (#743).** `tool_define` now requires every registration to declare
  its dispatch backend via an `executor` field — `"harn"` (with a
  callable `handler`), `"host_bridge"` (with a `host_capability:
  "cap.op"` binding), `"mcp_server"` (with a `mcp_server: "<name>"`
  binding), or `"provider_native"`. When `executor` is omitted, a
  registration with a handler is treated as `"harn"` for back-compat;
  a registration with neither handler nor executor is rejected at
  definition time with a clear error pointing at the missing backend
  rather than the historical `[builtin_call] unhandled: <name>`
  runtime failure. `agent_loop` re-validates at startup and refuses
  to run a registry with any handlerless, undeclared tool.
  Dispatch tags `tool_call_update.executor` on the ACP wire from the
  declared backend (host-bridge tools always tag as `HostBridge`, etc.),
  so clients no longer have to infer "via mcp:linear" / "via host
  bridge" from `_mcp_server` annotations alone. `harn check` validates
  `executor: "host_bridge"` `host_capability` bindings against the
  same capability map `host_call(...)` uses, so unknown bindings
  surface during the static check rather than at first model call.
  See `spec/HARN_SPEC.md#tool-execution-backend-executor` and
  `docs/llm/harn-quickref.md#tool-executor-declarations` for the new
  contract; the VM-stdlib short-circuit set (`read_file`,
  `list_directory`) continues to dispatch through `handle_tool_locally`
  and accepts `executor: "harn"` without a registered handler.

### Added

- **`llm_call_structured_result` diagnostic envelope (#744).** Adds a
  third structured-output entry point that returns a stable
  `{ok, data, raw_text, error, error_category, attempts, repaired,
  extracted_json, usage, model, provider}` dict so production agent
  pipelines can preserve raw model text, attempt counts, and
  validation / repair state without hand-rolling
  `safe_parse → json_extract → repair → schema_check` chains. Never
  throws on transport / schema failures — `error_category` ∈
  `transport`-class categories (`rate_limit`, `timeout`, `auth`, …) or
  `missing_json` / `schema_validation` / `repair_failed`. Options
  accept a `repair: {enabled, ...llm_call_overrides}` block: an
  optional one-shot repair pass that reissues a separate LLM call on
  malformed JSON only and is skipped on transport failures.
  Schema-as-type narrowing flows through the envelope's `data` field
  (`Schema<T>` → `data: T | nil`). Bridge and non-bridge registration
  paths share one implementation so the envelope shape is identical
  across ACP and direct execution. The
  `crates/harn-vm/src/llm/structured_envelope.rs` module owns the
  envelope builder; `execute_llm_call` is refactored to share its
  schema-retry loop with the envelope path via the new
  `execute_schema_retry_loop` helper. Conformance fixtures
  (`llm_call_structured_result_*.harn`) cover clean JSON, fenced JSON,
  prose-wrapped JSON, schema retry recovery, schema-validation
  failure, repair success, repair failure, transport failure, and
  schema-as-type narrowing.
- **Predicate-count explosion limits for Flow slice union (#733).** A
  cross-directory slice that touches many leaves with sibling-specific
  `invariants.harn` files used to silently fan out into hundreds or
  thousands of predicates, and Ship Captain would just pay the serial
  evaluation cost. `PredicateCeiling::default()` (256 soft / 1024 hard)
  now fronts evaluation: at the soft ceiling Flow returns
  `RequireApproval` routed to the `flow-platform` role; at the hard
  ceiling it returns `Block` with the stable code
  `predicate_count_explosion`. Both verdicts carry a structured
  violation listing the count, threshold, and top-contributing
  directories so operators can see where to prune. `harn flow ship watch`
  surfaces the outcome under
  `predicate_validation.ceiling` and propagates the level into
  `mock_pr.validation_status`. Calibration data lives in the new
  `flow_predicate_union` criterion bench: union resolve and the ceiling
  check stay microsecond-scale even at ~2000 predicates, so the limit is
  operational (the 50ms-per-predicate evaluation budget), not perf.
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
- **Package-root prompt asset paths (#742).** `render`, `render_prompt`,
  the `template.render` host capability, and `{{ include "..." }}`
  directives now accept two refactor-safe forms in addition to plain
  source-relative paths: `@/<rel>` resolves from the calling file's
  project root (the nearest `harn.toml` ancestor), and
  `@<alias>/<rel>` resolves from a new `[asset_roots]` table in
  `harn.toml`. Both forms reject `..` segments so they cannot escape
  the project root. `harn check` validates `@`-paths during preflight,
  `harn contracts bundle` records them under `prompt_assets`, and the
  Harn LSP go-to-definition jumps from a literal
  `render_prompt("@/...")` argument to the target prompt file. Plain
  paths keep their existing source-relative behavior; back-compat is
  exact.

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
