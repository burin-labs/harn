# Composable tool middleware

Harn's `agent_loop` exposes two composable seams for **tool calls** —
mirrors of the `llm_caller` seam at the model boundary, but operating
on tool execution. They let harness authors transform tools and the
calls against them without forking individual tool definitions or
patching the runtime.

| Seam | Stage | Hook | Use cases |
|------|-------|------|-----------|
| **Schema-time** | Before the model sees a registry | `tools_use_middleware(registry, transform)` | Augment input schemas (force a `reason` arg, inject a `dry_run` flag), drop tools, rewrite descriptions. |
| **Execution-time** | Around every tool dispatch | `agent_loop({tool_caller: caller})` | Audit logs, consent prompts, dry-run preview, redaction, idempotency, rate-limit, telemetry. |

Both seams compose. The execution-time seam runs *every* dispatch
regardless of executor (`harn`, `host_bridge`, `mcp_server`,
`provider_native`), so a single `with_audit_log` reaches script tools,
host-bridge tools (e.g. burin-code), and MCP-served tools uniformly.

The user-facing module is `std/llm/tool_middleware`.

## Why this exists

A motivating use case: force every tool call to provide a `reason`
parameter explaining *why* it's invoking the tool. The harness
benefits in three ways:

1. **User-facing summary.** Hosts render
   "Searched codebase to find rate limiter middleware" instead of a
   generic "ran 3 tools" counter.
2. **Better model reasoning.** Forcing the model to articulate intent
   often improves quality on complex agentic tasks.
3. **Audit trail.** Every tool call carries a structured "why" that
   downstream observability tools (Langfuse, Datadog, Honeycomb) can
   index.

The same primitive — wrap tool calls — supports consent, dry-run,
rate-limit, redaction, idempotency, and telemetry middleware. Every
useful middleware someone writes becomes a building block others can
stack.

## Caller contract

The execution-time middleware closure has signature:

```harn,ignore
fn(call, next) -> result_dict
//   call = {
//     tool_name:          string,
//     tool_args:          dict,
//     call_id:            string,
//     declared_executor:  "harn" | "host_bridge" | "mcp_server" |
//                         "provider_native" | nil,
//     schema:             dict | nil,    // input parameters JSON schema
//     description:        string,        // tool description
//     turn:               {iteration: int, session_id: string},
//   }
//   next: fn(call) -> result_dict        // bottom of the stack runs
//                                        // the runtime default dispatch
```

Each layer can:

- **Inspect / observe** the inbound call and outbound result.
- **Mutate args**: call `next(call + {tool_args: rewritten})`.
- **Short-circuit**: return a result dict without calling `next`.
- **Augment audit data**: attach an `audit` key to the result dict;
  the runtime fans it out as a `tool_call_audit` AgentEvent.

The result dict mirrors the dispatch result shape:

```text
{
  ok, status, tool_name, tool_call_id, arguments,
  result, rendered_result, observation,
  error, error_category, executor, approval, execution_duration_ms,
  audit?,
}
```

## The `audit` field convention

Middleware-attached audit metadata is intentionally free-form JSON, but
the field names align with prevailing specs where they exist:

| Field | Source | Meaning |
|-------|--------|---------|
| `summary?` | ACP `title`, OpenAI Responses `summary_text` | User-facing one-liner ("Searched codebase…") |
| `description?` | OTel `gen_ai.tool.description`, LangChain | Longer free-form rationale |
| `kind?` | ACP ToolCall.kind | One of `read`/`edit`/`delete`/`move`/`search`/`execute`/`think`/`fetch`/`other` |
| `hints?` | MCP tool annotations | `{read_only?, destructive?, idempotent?, open_world?}` |
| `consent?` | (coined; ACP/MCP keep this off the call object) | `{decision, decided_by, decided_at}` |
| `scope?` | (coined; mirrors `PersonaRuntimeBinding.stages`) | `{stage, allowed_tools?, side_effect_level?}` — the scoped capability surface this call ran under |
| `layers?` | (coined) | `[{name, status, started_at, ended_at, error?}]` per-layer audit log |
| `metadata?` | A2A `metadata`, LangChain | Free-form extension slot |

These names are conventions, not requirements — middleware authors are
free to invent new keys. Use the conventional names where they fit so
that bridge-out / OTel exporters / ACP gateway adapters work cleanly.

## Reserved status values

When a layer short-circuits, prefer one of these `status` values so
composition stays predictable:

`"ok"`, `"tool_not_found"`, `"schema_violation"`, `"consent_denied"`,
`"policy_blocked"`, `"scope_violation"`, `"executor_error"`,
`"redacted"`, `"dry_run"`, `"rate_limited"`, `"exception"`,
`"tool_middleware_exception"`.

## Bundled middleware

`std/llm/tool_middleware` ships the following middleware. Each is
~10-30 lines of Harn — they're examples as much as they're useful in
their own right.

### `with_required_reason(opts?) -> {schema_transform, caller}`

The originating use case. Returns a paired schema decorator + execution
caller. By default it forces every tool call to provide a non-empty
`reason` (or a custom-named field), strips it before delegating to
`next`, and surfaces it on `audit.summary`.

Options: `field` (default `"reason"`), `description`, `strip` (bool,
default true), `audit_key` (default `"summary"`), `min_length` (default
1), `on_missing` (`"reject"` (default) or `"fill_blank"`), and
`schema_required` (bool, default true).

Set `schema_required: false` with `on_missing: "fill_blank"` when a host
wants the audit field advertised but cannot trust provider-native tool
schema enforcement. This keeps the real tool arguments intact even when
the model omits the synthetic `reason` field, while still recording
`"(no reason given)"` in the audit summary.

```harn,ignore
let mw = with_required_reason({schema_required: false})
let registry = tools_use_middleware(my_registry, mw.schema_transform)
agent_loop(task, system, {tools: registry, tool_caller: mw.caller})
```

`schema_required: false` keeps runtime validation aligned with the
default `strip: true`; the middleware still rejects calls that omit the
reason before the tool handler runs.

### `with_audit_log(sink_or_options) -> caller`

Builds one typed `ToolCallReceipt` per tool call after the call
completes. Receipts include the required-reason summary, status,
executor, timing, model/provider, audit metadata, and SHA-256 hashes of
canonicalized args/results instead of raw payloads.

`sink_or_options` accepts a callable sink, `"local"`, `"cloud"`,
`"both"`, or `{sink, redact}`. Local receipts append to
`.harn/receipts/<session_id>.jsonl`; cloud receipts mirror through the
host event bridge; `both` writes local first and mirrors the same
receipt. `redact` is a list of argument keys removed before `args_hash`
is computed.

```harn,ignore
let caller = compose_tool_callers([
  with_audit_log({sink: "both", redact: ["token", "content"]}),
  with_required_reason({schema_required: false}).caller,
])
```

For explicit file routing, `local_receipt_sink(session_id)` returns the
same append-only JSONL sink used by `sink: "local"`.

### `with_consent(prompt_fn) -> caller`

`prompt_fn(call) -> bool | dict` is consulted before each tool call.
Denied calls short-circuit with `consent_denied`; approved calls
proceed and record the decision in `audit.consent`. Pair with the host
UX (e.g. burin-code's approval modal) for destructive tools.

### `with_scoped_executor(opts) -> caller`

Narrows the active `CapabilityPolicy` for the duration of one tool
dispatch. Wraps the downstream chain in `with_execution_policy(...)`
so the runtime's existing `enforce_current_policy_for_tool` machinery
(capability ceilings, side-effect ceiling, tool-arg constraints) sees
the scoped policy as the top of the stack — the scoped policy is
intersected with the ambient policy, so a stage can only tighten the
surface, never widen it. A preemptive tool-name check short-circuits
with `status: "scope_violation"` so the audit chain captures the stage
label even when the wrapped dispatch never runs.

Compose this **outside** binder / consent layers so it can reject
before either does expensive work:

```harn,ignore
let caller = compose_tool_callers([
  with_audit_log({...}),
  with_scoped_executor({stage: "research", allowed_tools: ["search_files", "read_file"]}),
  with_consent(prompt),
  default_tool_caller(),
])
```

Options:

- `stage` (default `"scoped"`) — surfaced on `audit.scope.stage` and in
  layer/error messages.
- `allowed_tools` (list of tool names; empty / missing skips the
  preemptive check and any tool-surface restriction).
- `side_effect_level` (`"none"` / `"read_only"` / `"workspace_write"` /
  `"process_exec"` / `"network"`; tightens the ambient ceiling).
- `capabilities` (capability → operation allowlist; same shape as
  `CapabilityPolicy.capabilities`).
- `on_violation` (`"reject"` (default) → short-circuit with
  `scope_violation` and a typed receipt; `"raise"` → throw so the agent
  loop's `try { ... }` can react).

Companion to per-stage persona declarations (`PersonaRuntimeBinding.stages`):
the persona runtime auto-installs stage policies at step boundaries,
while this middleware lets standalone tool callers narrow the surface
without declaring a full persona manifest.

### `with_dry_run(opts?) -> caller`

Never invokes `next` — short-circuits with a synthetic OK result
tagged `status: "dry_run"`. Useful for previewing a tool sequence
without side-effects. Options: `only` (whitelist) and `except`
(blacklist).

This is the userspace seam for **crystallization shadow runs**: when
`shadow_replay_bundle` (orchestration/crystallize) re-executes a
captured workflow to confirm a candidate's side-effect signature
hasn't drifted, wrap the tool caller in `with_dry_run({except:
[...known-pure-tools]})` so destructive ops are neutralized while
read-only ones still surface real results.

### `with_redaction(redactor) -> caller`

Applied twice: once on inbound args, once on outbound result.
`redactor({phase: "in"|"out", tool_name, args, result}) ->
{args?, result?, redacted_fields?}`. Records redacted-field names in
`audit.metadata`.

### `with_idempotency(key_fn, opts?) -> caller`

Caches successful tool results keyed by `key_fn(call) -> string`,
backed by `std/cache` so the cache outlives the caller closure. Repeat
queries within the TTL reuse cached results.

### `with_rate_limit(opts) -> caller`

Caps the total number of tool calls processed by this caller. Once
`max_calls` is hit, further calls short-circuit with `rate_limited`.

### `with_telemetry(sink_or_opts) -> caller`

Emits a standardized tool-call span for every dispatch and fans it out
to one or more sinks. Accepts a callable `fn(span)`, a built-in name
(`"langfuse"`, `"otel"`, `"stderr"`, `"noop"`), or a config dict with
`sink:` / `sinks:`. The span shape mirrors `gen_ai.tool.*` attributes
and exposes timing, executor, args hash, layered child spans, and
dispatched / result / scope-violation events. Full schema and built-in
sink reference: [Tool-call spans](../observability/tool-call-spans.md).

### `with_summary(format_fn) -> caller`

Generates a user-facing one-liner via `format_fn(call, result) -> string`,
populating `audit.summary` (the ACP/OpenAI convention slot).

### `with_handoff_artifact(opts?) -> caller`

When a tool's result carries a handoff payload (under `__handoff` /
`handoff` by default, or a custom-detected key), normalizes it through
`handoff(...)` and surfaces the typed record on `audit.handoff`. The
optional `sink(record, call)` callback fires once per emitted handoff
for side-effecting persistence. Pairs with `std/handoffs` for the
typed handoff schema.

Options: `sink` (callback), `detect` (custom locator),
`keys` (extra result keys to inspect, default `["__handoff", "handoff"]`),
`source` (override `source_persona` if the tool didn't set one),
`strict` (throw on malformed payloads, default `false`).

### `with_timeout(opts) -> caller`

Caps wall-clock time per tool call. Calls inside the budget pass
through with `audit.layers[…].status == "ok"`. Calls that breach the
budget surface `error_category: "timeout"` and `status: "timeout"` on
the layer log. The middleware does *not* cancel the in-flight dispatch
(hard cancellation belongs in `agent_loop({deadline_ms})`); it
observes the breach so upstream layers can react.

Options: `max_ms` (required, non-negative int), `per_tool`
(`{tool_name: override_max_ms}`), `message` (override error message).

## Composing

`compose_tool_callers([outer, ..., inner])` returns one caller that
runs the wrappers right-to-left: the leftmost wrapper is the
outermost. This mirrors `compose` in `std/llm/handlers`.

```harn,ignore
let caller = compose_tool_callers([
  with_audit_log({sink: "both", redact: ["token", "content"]}),
  with_consent(prompt),
  with_redaction(redactor),
  with_required_reason({schema_required: false}).caller,
])
```

## Captain recipe — full governance stack

The persona platform's captains (`merge_captain`, `review_captain`,
`oncall_captain`) all want the same substrate: every tool call yields
a structured audit record, destructive ops gate on consent, the loop
caps at a tool budget, and handoff payloads surface as typed records
on the receipts ledger. One stack covers all of it.

```harn,ignore
import {
  compose_tool_callers,
  with_audit_log,
  with_consent,
  with_dry_run,
  with_handoff_artifact,
  with_idempotency,
  with_rate_limit,
  with_redaction,
  with_required_reason,
  with_summary,
  with_telemetry,
} from "std/llm/tool_middleware"

let reason_mw = with_required_reason({schema_required: false})

let captain_tool_caller = compose_tool_callers([
  with_audit_log({sink: "both", redact: ["token", "content"]}), // typed tool receipts
  with_telemetry({sink: "langfuse", project: "harn-dev"}), // tool-call spans
  with_summary({ call, _r -> describe(call) }), // user-facing one-liner
  with_consent(persona.autonomy_policy),       // act_with_approval gate
  reason_mw.caller,                            // require `reason` arg
  with_redaction(unified_redaction_policy),    // strip secrets
  with_handoff_artifact({sink: handoff_emitter}), // typed handoff records
  with_idempotency(per_tool_idempotency_keyer),
  with_rate_limit({max_calls: persona.tool_budget}),
  with_dry_run({only: persona.shadow_tools}),  // crystallization shadow runs
])

let registry = tools_use_middleware(my_registry, reason_mw.schema_transform)

agent_loop(task, system, {
  tools: registry,
  tool_caller: captain_tool_caller,
})
```

Order matters: the leftmost (outermost) wrappers see every call,
including those short-circuited by inner layers, so put audit and
telemetry first. The required-reason / consent / redaction layers go
in the middle, and the rate-limit / dry-run gates are innermost so the
audit log sees what the runtime actually attempted.

## Gotchas

1. **Closures capture by value.** Don't try to share a free-form dict
   across calls of a stateful middleware — the captured reference is
   frozen. Use `atomic(0)` for integer counters or `std/cache` for
   richer state. See the existing `std/llm/handlers::with_budget` for
   the standard pattern.
2. **Short-circuiting must produce a complete result dict.** The
   downstream `agent_session_record_tool_results` expects the standard
   shape (`tool_name`, `ok` or `success` or `status`, `observation` or
   `rendered_result` or `output` or `content`). Use
   `__tool_mw_short_circuit` patterns or the bundled middleware as a
   template.
3. **Sequential dispatch with middleware.** When `tool_caller` is set,
   the runtime dispatches tool calls sequentially in source order so
   audit-log / consent / redaction observe a deterministic sequence.
   Without middleware, the runtime parallelizes the read-only prefix
   (the historical optimization). Authors that want concurrency under
   middleware can wrap the inner call with their own `parallel each`.
4. **Schema decorators should be additive.** `tool_inject_param`
   leaves an existing parameter untouched if it's already declared so
   layered middleware (e.g. multiple injects of the same field) is
   idempotent.
5. **The `tool_call_audit` AgentEvent is fired only when middleware
   sets `result.audit`.** No middleware → no event. This keeps the
   wire stream clean for hosts that don't subscribe.
6. **`with_required_reason({strip: true})` + schema_transform + agent_loop.**
   The runtime's `validate_tool_args` runs *after* the middleware strips
   `reason`, so combining the schema decorator (which marks `reason`
   required) with `strip: true` will reject every call with a
   "missing required parameter: reason" error. Either:
   - Use `strip: true` *without* `schema_transform` — the model is told
     about `reason` via the system prompt or live-call instructions, and
     the natural tool schemas don't list it (the middleware strips it
     before the handler runs). This is the pattern used by the bundled
     conformance tests.
   - Or use `schema_transform` with `strip: false` — `reason` flows
     through to the handler, which is responsible for ignoring it.

## Wire format

Each middleware-attached audit blob is also emitted as a
`tool_call_audit` `AgentEvent` so live ACP/A2A consumers can render
chips alongside the standard `tool_call_update` stream:

```json
{
  "type": "tool_call_audit",
  "session_id": "…",
  "tool_call_id": "…",
  "tool_name": "search_files",
  "audit": {
    "summary": "Searched codebase to find rate limiter",
    "kind": "search",
    "consent": {"decision": "approved", "decided_by": "auto"},
    "layers": [
      {"name": "with_required_reason", "status": "ok"},
      {"name": "with_consent", "status": "approved"}
    ]
  }
}
```

## See also

- `docs/src/stdlib/llm-handlers.md` — the parallel seam at the model
  boundary.
- `docs/llm/harn-quickref.md` "Composable tool middleware" section —
  the autoloaded one-pager.
- `crates/harn-stdlib/src/stdlib/llm/tool_middleware.harn` — the
  source, with full per-function comments.
- `conformance/tests/integration/tool_middleware_*.harn` — executable
  examples covering the primitives, `with_required_reason`,
  `with_consent`, `with_dry_run`, and the agent_loop integration.
