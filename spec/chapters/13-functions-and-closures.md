## Functions and closures

### fn declarations

```harn
fn name(param1, param2) {
  return param1 + param2
}
```

Declares a named function. Equivalent to `let name = { param1, param2 -> ... }`.
The function captures the lexical scope at definition time.

### Default parameters

Parameters may have default values using `= expr`. Required parameters must
come before optional (defaulted) parameters. Defaults are evaluated fresh at
each call site (not memoized at definition time). Any expression is valid as
a default — not just literals.

```harn
fn greet(name, greeting = "hello") {
  log("${greeting}, ${name}!")
}
greet("world")           // "hello, world!"
greet("world", "hi")     // "hi, world!"

fn config(host = "localhost", port = 8080, debug = false) {
  // all params optional
}

const add = { x, y = 10 -> x + y }  // closures support defaults too
```

Explicit `nil` counts as a provided argument (does NOT trigger the default).
Arguments are positional — fill left to right, only trailing defaults can
be omitted.

### tool declarations

```harn
tool read_text(path: string, encoding: string = "utf-8") -> string {
  description "Read a file from the filesystem"
  read_file(path)
}

tool search_files(query: string, file_glob: string = "*.py") -> string {
  description "Search files matching an optional glob"
  "..."
}
```

Declares a named tool and registers it with a tool registry. The body is
compiled as a closure and attached as the tool's handler. An optional
`description` metadata string may appear as the first statement in the body.

Annotated tool parameter and return types are lowered into the same schema
model used by runtime validation and structured LLM I/O. Primitive types map to
their JSON Schema equivalents, while nested shapes, `list<T>`,
`dict<string, V>`, and unions produce nested schema objects. Parameters with
default values are emitted as optional schema fields (`required: false`) and
include their `default` value in the generated tool registry entry.

The result of a `tool` declaration is a tool registry dict (the return
value of `tool_define`). Multiple `tool` declarations accumulate into
separate registries; use `tool_registry()` and `tool_define(...)` for
multi-tool registries.

Like `fn`, `tool` may be prefixed with `pub`.

#### Tool execution backend (`executor`)

Every entry registered through `tool_define` declares its execution
backend. The runtime uses the declared executor to dispatch the call
and to populate `tool_call_update.executor` on the ACP wire so clients
can route badges and latency by transport.

| `executor` | Required companion field | Backend |
|---|---|---|
| `"harn"` *(alias `"harn_builtin"`)* | `handler` (closure) | In-VM Harn handler. The VM stdlib short-circuits `read_file` / `list_directory` even without a registered handler. |
| `"host_bridge"` | `host_capability: "cap.op"` | Host shell `builtin_call` bridge. `harn check` validates the binding against the host capability manifest. |
| `"mcp_server"` | `mcp_server: "<server_name>"` | Configured MCP server. Tools sourced from `mcp_list_tools` carry the equivalent `_mcp_server` annotation. |
| `"provider_native"` | *(none)* | Provider-side server tools (e.g. OpenAI Responses-API). Never dispatched locally. |

`tool_define` rejects invalid combinations (`executor: "host_bridge"`
plus a handler, `executor: "harn"` without a handler outside the
VM-stdlib short-circuit set, missing `host_capability` /
`mcp_server`, unknown executor strings) at definition time. When
`executor` is omitted, the registration is interpreted as
`"harn"` if a handler is present, and rejected otherwise — eliminating
the historical `[builtin_call] unhandled: <name>` runtime failure.

`agent_loop` re-validates at startup and refuses to run if any tool in
the bound registry has no executable backend. The error message names
the offending tool and the documented set of executors.

```harn
const registry = tool_registry()
registry = tool_define(registry, "ask_user", "Ask the user", {
  parameters: {prompt: "string"},
  executor: "host_bridge",
  host_capability: "interaction.ask",
})
```

Registries exposed through `mcp_tools(registry)` should declare MCP
tool annotations on each `tool_define` entry. The MCP server forwards
recognized `annotations` fields (`title`, `readOnlyHint`,
`destructiveHint`, `idempotentHint`, and `openWorldHint`) plus
top-level `title` and `icons` metadata into the `tools/list` response.
`harn lint` warns when a registry passed to `mcp_tools` contains a
`tool_define` entry without annotations.

```harn
registry = tool_define(registry, "repo.status", "Read repository status", {
  parameters: {},
  title: "Repository Status",
  handler: { _args -> git.status() },
  annotations: {
    title: "Repository Status",
    readOnlyHint: true,
    destructiveHint: false,
    idempotentHint: true,
    openWorldHint: false,
  },
  icons: [{src: "https://example.com/repo-status.png", mimeType: "image/png"}],
})
```

#### Tool surface validation

`tool_surface_validate(surface, options?)` validates a tool-calling
surface before a model call. The surface may include `tools`, `policy`,
`approval_policy`, `system` / `prompt` / `prompts`, and `tool_search`.
It returns `{valid, diagnostics}` where each diagnostic has a stable
`code`, `severity`, and `message`.

`agent_loop` runs the validator at startup. Warning diagnostics are
logged; error diagnostics abort the loop before the first model call.
`workflow_validate` and `workflow_policy_report` include the same
diagnostics for workflow and stage surfaces.

#### Agent loop tool narrowing

`agent_loop` narrows the model-visible tool surface between turns by
default. The `tool_surface_narrowing` option accepts `false` to disable,
`true` to use defaults, or a dict with `enabled`, `window_turns`,
`hard_keep`, `mode`, `prune_classes`, `keep_classes`, and
`unknown_tool_policy`. After `window_turns` observed turns, tools in
prunable classes that were not attempted in that rolling window are removed
from the next model call unless listed in `hard_keep`. The default window
is five turns.

Narrowing is usage-based only; it does not inspect file extensions,
languages, or project metadata. The default `mode: "safe"` prunes only
`read_only` tools and keeps `mutating`, `approval`, `session_control`,
`progress`, `result_polling`, and `unknown` tools by class. Host/custom
tools with missing side-effect annotations are therefore kept by default.
Set `mode: "aggressive"` to opt into usage-only pruning across tool classes,
or override `prune_classes`, `keep_classes`, and `unknown_tool_policy` for a
custom policy. Host surfaces should annotate tools with
`annotations.side_effect_level` (`none`, `read_only`, `workspace_write`,
`process_exec`, or `network`) and `annotations.kind` so narrowing can classify
them intentionally.

Explicit skill activation widens back to the skill-scoped surface, after
which narrowing may run again. Every narrowing step emits a `skill_narrow`
agent event with `reason`, `removed_tools`, `remaining_tools`, `policy`,
`removed_tool_details`, and `kept_tool_details` so replayers and hosts can
explain why a tool did or did not remain visible.

#### Agent loop completion gates

`agent_loop` may gate a pending stop with `verify_completion`,
`verify_completion_judge`, or `done_judge`. `verify_completion` is a Harn
closure hook. `verify_completion_judge` runs the built-in structured judge for
any natural stop. `done_judge` runs after a native-tool loop naturally
completes or after the model emits the configured done sentinel. The structured
judge returns `verdict: "done" | "continue"` plus optional `reasoning` and
`next_step`, and injects judge feedback before continuing when the verdict
rejects completion. Each built-in judge call and deterministic
`verify_completion` decision emits `JudgeDecision {session_id, iteration,
verdict, reasoning, next_step, judge_duration_ms, trigger?, reason?, confirm?,
converted_from?, escalation_recommended?, escalation_target?}`. The optional
`trigger` is `"stalled"` when a
`done_judge.cadence.when: "stalled"` judge fires from an
`agent_loop_stall_warning`; a `done` verdict stops the loop with
`stalled_done_judge` before the repeated tool call dispatches, and a
`continue` verdict leaves the stall feedback path in place.

#### Agent progress events

`std/agent/progress` exposes `agent_progress(input)` and
`agent_progress_tool(registry?, options?)` for live agent status. The
`agent_progress` input must contain a non-empty `message` or `entries`.
`entries` is a list of task-list dicts with `content`, `status`, and optional
`priority`; `replace` defaults to `true`, and `metadata` defaults to `{}`.

At runtime `agent_progress` emits the `progress_reported` agent event for the
current agent session. ACP bridges encode task-list entries as canonical
`session/update` payloads with `sessionUpdate: "plan"` and full-replacement
`entries`; message-only reports use Harn progress narration. A2A bridges encode
progress reports as non-terminal `TaskStatusUpdateEvent` messages with
`status.state = "working"` and no push-delivery side effect.

`agent_progress_tool` installs a model-facing tool that calls
`agent_progress`. `agent_loop(..., {progress_tool: true})` installs the default
tool; dict form may override `name`, `description`, and
`system_prompt_nudge`. Progress reports are intended for meaningful sub-step
completion or plan changes, not fixed timer ticks.

For hosts and reusable harness code, `std/agent/progress` exposes matching
schemas and validators for the normalized payload/event/config shapes:
`agent_progress_payload_schema`, `agent_progress_event_schema`,
`agent_progress_tool_config_schema`, their paired `*_report` / `*_value`
helpers, and `agent_progress_tool_config_normalize` for defaulted config.

#### Agent scratchpad

`agent_loop(..., {scratchpad: true})` initializes a session-local
`harn.agent_scratchpad.v1` value with `goals`, `open_items`, `facts`, and
`refs`. The current scratchpad is recited as a prompt-tail fragment every turn
and, by default, reorganized every three continuing turns through a
structured-output LLM call. Dict form may set `enabled`, `recite`,
`reorganize_every`, `max_recent_messages`, `schema_retries`, `initial`, and
`reorganizer`; the reorganizer options may select a different provider/model
from the worker.

Reorganization is required to keep heavy content by reference. Returned facts
must cite `source_ref` values present in the current scratchpad or recent
session messages; a reorganization that invents refs is rejected and the prior
scratchpad remains active. Successful and failed reorganization attempts emit
`agent_scratchpad_reorganization` events with status and count metadata.

`agent_turn(prompt, options?)` is the high-level wrapper for a single complete
agent request. It uses `agent_loop`, folds `options.system` into the system
prompt with generic progress guidance, defaults to explicit
`loop_until_done` completion, and requires `done_judge` (omitted means the
default judge).

Execute tools that can emit large result artifacts declare this in
`ToolAnnotations`:

```harn,ignore
annotations: {
  kind: "execute",
  side_effect_level: "process_exec",
  emits_artifacts: true,
  result_readers: ["read_command_output"],
}
```

Set `inline_result: true` instead of `result_readers` when the tool
always returns complete inline output. Prompt scans ignore fenced code
blocks and support `harn-tool-surface: ignore-line`,
`harn-tool-surface: ignore-next-line`, and
`harn-tool-surface: ignore-start` / `ignore-end` comments for
historical examples.

#### Command execution policy hooks

Command execution policy is a first-class Harn value created with
`command_policy(config)`. A policy can be installed directly with
`command_policy_push(policy)` / `command_policy_pop()` or scoped to
`agent_loop` with `command_policy: policy` or
`policy: { command_policy: policy }`. While installed,
`host_call("process.exec", ...)` builds a normalized command context,
runs deterministic risk classification, executes the policy pre-hook
before spawning, and records decisions in the returned command envelope
under `command_policy`.

`command_policy` config fields:

| Field | Description |
|---|---|
| `tools` | Agent-visible tool names the policy is intended to guard when threaded through a workflow or harness |
| `workspace_roots` | Workspace roots used for outside-workspace classification |
| `default_shell_mode` | Policy hint such as `"argv_only"` for harness/tool authors |
| `deny_patterns` | Glob/substring patterns that block before spawn |
| `require_approval` | Risk labels that block with a require-approval decision unless the host grants approval |
| `pre` | Closure receiving normalized command context; returns `nil`, `{deny}`, `{require_approval}`, `{rewrite}`, `{dry_run}`, or `{explain_only}` |
| `post` | Closure receiving command context plus the result envelope; may return `{result}`, `{feedback}`, or audit annotations |
| `allow_recursive` | Allows policy hooks to call the command runner recursively; default is `false` |

The pre-hook context includes the resolved request (`mode`, `argv`,
`command`, `shell`, `cwd`, redacted env diff, stdin size/hash,
timeout), active cwd, workspace roots, policy ceiling, tool
annotations, redacted transcript slots, caller metadata, and
deterministic scan results. Rewrite is constrained to command-runner
request fields such as `argv`, `command`, `cwd`, `env`, `timeout_ms`,
`shell`, and capture settings.

Standard deterministic risk labels include `destructive`,
`write_intent`, `outside_workspace`, `curl_pipe_shell`,
`credential_file_read`, `network_exfil`, `sudo`, `package_install`,
`git_force_push`, and `process_kill`.
`command_llm_risk_scan(ctx, options?)` returns the same structured
shape (`risk_labels`, `confidence`, `rationale`,
`recommended_action`) with redacted scan options; it is safe to use in
tests without making a network call.

#### Deferred tool loading (`defer_loading`)

A tool registered through `tool_define` may set `defer_loading: true`
in its config dict. Deferred tools keep their schema out of the model's
context on each LLM call until a tool-search call surfaces them.

```harn
fn admin(token) { log(token) }

const registry = tool_registry()
registry = tool_define(registry, "rare_admin_action", "...", {
  parameters: {token: {type: "string"}},
  defer_loading: true,
  handler: { args -> admin(args.token) },
})
```

`defer_loading` is validated as a bool at registration time — typos like
`defer_loading: "yes"` raise at `tool_define` rather than silently
falling back to eager loading.

Deferred tools are only materialised on the wire when the call opts
into `tool_search` (see the `llm_call` option of the same name and
`docs/src/llm-and-agents.md`). Harn supports two native backends plus a
provider-agnostic client fallback:

- **Anthropic Claude Opus/Sonnet 4.0+ and Haiku 4.5+** — Harn emits
  `defer_loading: true` on each deferred tool and prepends the
  `tool_search_tool_{bm25,regex}_20251119` meta-tool. Anthropic keeps
  deferred schemas in the API prefix (prompt caching stays warm) but
  out of the model's context.
- **OpenAI GPT 5.4+ (Responses API)** — Harn emits
  `defer_loading: true` on each deferred tool and prepends
  `{"type": "tool_search", "mode": "hosted"}` to the tools array.
  OpenRouter, Together, Groq, DeepSeek, Fireworks, HuggingFace, and
  local vLLM inherit the capability when their routed model matches
  `gpt-5.4+`.
- **Everyone else (and any of the above on older models)** — Harn
  injects a synthetic `__harn_tool_search` tool and runs the configured
  client strategy in Harn/VM space, promoting matching deferred tools into the
  next turn's schema list. Built-in client strategies are `bm25`, `regex`, and
  `hybrid`; custom scorer closures and `{handler}` strategy objects can replace
  the search step completely.

Tool entries may also set `namespace: "<label>"` to group deferred tools
for the OpenAI meta-tool's `namespaces` field. The field is a harmless
passthrough on Anthropic — ignored by the API, preserved in replay.

`mode: "native"` refuses to silently downgrade and errors when the
active (provider, model) pair is not natively capable; `mode: "client"`
forces the fallback everywhere; `mode: "auto"` (default) picks native
when available.

The per-provider / per-model capability table that gates native
`tool_search`, `defer_loading`, prompt caching, and typed thinking modes
is a shipped TOML matrix overridable per-project via
`[[capabilities.provider.<name>]]` in `harn.toml`. Scripts query the
effective matrix at runtime with:

```harn
const caps = provider_capabilities("anthropic", "claude-opus-4-7")
// {
//   provider, model, native_tools, text_tool_wire_format_supported,
//   preferred_tool_format: "native" | "text",
//   tool_mode_parity: "interchangeable" | "unknown" | "native_unreliable" | "text_unreliable" | "native_only" | "text_only" | "unsupported",
//   tool_mode_parity_notes: string | nil,
//   tools, defer_loading,
//   tool_search: [string], max_tools: int | nil,
//   prompt_caching, thinking, thinking_modes: [string],
//   reasoning_effort_supported, reasoning_none_supported,
//   message_wire_format, native_tool_wire_format,
//   prefers_xml_scaffolding, prefers_markdown_scaffolding,
//   structured_output_mode: "native_json" | "delimited" | "xml_tagged" | "none",
//   supports_assistant_prefill, prefers_role_developer, prefers_xml_tools,
//   thinking_block_style: "none" | "thinking_blocks" | "reasoning_summary" | "inline",
// }
```

The `provider_capabilities_install(toml_src)` and
`provider_capabilities_clear()` builtins let scripts install and
revert overrides in-process for cases where editing the manifest is
awkward (runtime proxy detection, conformance test setup). See
`docs/src/llm/providers.md#capability-matrix--harntoml-overrides`
for the rule schema.

### skill declarations

```harn
pub skill deploy {
  description "Deploy the application to production"
  when_to_use "User says deploy/ship/release"
  invocation "explicit"
  paths ["infra/**", "Dockerfile"]
  allowed_tools ["bash", "git"]
  model "claude-opus-4-7"
  effort "high"
  prompt "Follow the deployment runbook."

  on_activate fn() {
    log("deploy skill activated")
  }
  on_deactivate fn() {
    log("deploy skill deactivated")
  }
}
```

Declares a named skill and registers it with a skill registry. A skill
bundles metadata, tool references, MCP server lists, system-prompt
fragments, and auto-activation rules into a typed unit that hosts can
enumerate, select, and invoke.

Body entries are `<field_name> <expression>` pairs separated by
newlines or semicolons. The field name is an ordinary identifier (no keyword is
reserved), and the value is any expression — string literal, list
literal, identifier reference, dict literal, or fn-literal (for
lifecycle hooks). The compiler lowers the decl to:

```harn,ignore
skill_define(skill_registry(), NAME, { field: value, ... })
```

and binds the resulting registry dict to `NAME`, parallel to how
`tool NAME { ... }` works.

`skill_define` performs light value-shape validation on known keys:
`description`, `when_to_use`, `prompt`, `invocation`, `model`, `effort`
must be strings; `paths`, `allowed_tools`, `mcp` must be lists.
Mistyped values fail at registration rather than at use. Unknown keys
pass through unchanged to support integrator metadata.

Like `fn` and `tool`, `skill` may be prefixed with `pub` to export it
from the module. The registry-dict value is bound as a module-level
variable.

#### Skill registry operations

```harn
const reg = skill_registry()
const reg = skill_define(reg, "review", {
  description: "Code review",
  invocation: "auto",
  paths: ["src/**"],
})
skill_count(reg)           // int
skill_find(reg, "review")  // dict | nil
skill_list(reg)            // list (closure hooks stripped)
skill_select(reg, ["review"])
skill_remove(reg, "review")
skill_describe(reg)        // formatted string
```

`skill_list` strips closure-valued fields (lifecycle hooks) so its
output is safe to serialize. `skill_find` returns the full entry
including closures.

#### `@acp_skill` attribute

Functions can be promoted into skills via the `@acp_skill` attribute:

```harn,ignore
@acp_skill(name: "deploy", when_to_use: "User says deploy", invocation: "explicit")
pub fn deploy_run() { ... }
```

Attribute arguments populate the skill's metadata dict, and the
annotated function is registered as the skill's `on_activate`
lifecycle hook. Like `@acp_tool`, `@acp_skill` only applies to
function declarations; using it on other kinds of item is a compile
error.

### Closures

```harn
const f = { x -> x * 2 }
const g = { a, b -> a + b }
```

First-class values. When invoked, a child environment is created from the *captured*
environment (not the call-site environment), and parameters are bound as immutable bindings.

### Spread in function calls

The spread operator `...` expands a list into individual function arguments.
It can be used in both function calls and method calls:

```harn
fn add(a, b, c) {
  return a + b + c
}

const args = [1, 2, 3]
add(...args)           // equivalent to add(1, 2, 3)
```

Spread arguments can be mixed with regular arguments:

```harn
fn add(a, b, c) { return a + b + c }

const rest = [2, 3]
add(1, ...rest)        // equivalent to add(1, 2, 3)
```

Multiple spreads are allowed in a single call, and they can appear in any
position:

```harn
fn add(a, b, c) { return a + b + c }

const first = [1]
const last = [3]
add(...first, 2, ...last)   // equivalent to add(1, 2, 3)
```

At runtime the VM flattens all spread arguments into the argument list
before invoking the function. If the total number of arguments does not
match the function's parameter count, the usual arity error is produced.

### Return

`return value` inside a function/closure unwinds execution via
`HarnRuntimeError.returnValue`. The closure invocation catches this and returns the value.
`return` inside a pipeline terminates the pipeline.
