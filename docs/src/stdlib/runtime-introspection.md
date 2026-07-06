# Runtime introspection tools

Harness authors can put model identity and runtime facts in prompts, but
model-visible prose goes stale and models often answer identity questions
from their own training prior. The runtime-introspection bundle gives the
model a tool surface that reports the facts the runtime *actually*
resolved for the current turn — model id, provider, context window,
Harn release, host harness, capability matrix, and compaction policy.

It is **opt-in**. Minimal harnesses that never call
`runtime_introspection_tools(reg)` get no introspection surface at all;
hosts that want truthful answers wire it in explicitly.

## Quick start

```harn
import { runtime_introspection_tools } from "std/agent/introspection"

fn search_workspace(_args) {
  return "results"
}

pipeline answer_identity_questions(task) {
  let reg = tool_registry()
  reg = runtime_introspection_tools(reg)
  reg = tool_define(
    reg,
    "search_workspace",
    "Search the active workspace.",
    {
      handler: search_workspace,
      parameters: {query: {type: "string"}},
      returns: {type: "string"},
    },
  )

  const result = agent_loop(
    task,
    "You are a helpful coding assistant.",
    {tools: reg, model: "claude-opus-4-7"},
  )
  return result
}
```

When the model asks "what model am I?" it now emits
`current_model()` and gets back
`{model: "claude-opus-4-7", model_alias: nil, family: "anthropic-claude", tier: "high", resolved: true}`
instead of guessing from training data.

## Tools

Every tool is `executor: "harn"` and dispatches through the VM stdlib
short-circuit (no Harn closure allocated per call). Each returns a dict
with a `resolved` flag — `false` until the first `llm_call` on the
thread populates the snapshot.

| Tool                            | Returns                                                            |
| ------------------------------- | ------------------------------------------------------------------ |
| `current_model`                 | `{model, model_alias, family, tier, resolved}`                     |
| `current_provider`              | `{provider, tool_format, resolved}`                                |
| `current_context_window`        | `{context_window, runtime_context_window, resolved}` (token counts) |
| `current_harn_version`          | `{harn_version}` — embedded `HARN_VERSION` from the build          |
| `current_harness`               | `{harness}` — host shell embedding the runtime                     |
| `available_runtime_capabilities` | `{capabilities, resolved}` — provider/model capability matrix     |
| `current_compaction_policy`     | `{policy}` — transcript compaction policy (engine default today)   |

## `runtime_introspection_tools(registry, options?)`

Add the bundle to a registry. Idempotent — calling it twice does not
duplicate entries. Options:

- `{only: ["current_model", "current_provider"]}` — narrow the surface
  to specific tools.
- `{exclude: ["current_compaction_policy"]}` — drop specific tools. Use
  this when the model doesn't need everything (smaller schemas, fewer
  tokens spent on tool definitions).
- `only` and `exclude` are mutually exclusive; `only` wins when both
  are set.

## `runtime_introspection()`

A direct read of the underlying snapshot, exposed as a builtin for
tests, observability, and scripts that need the data without going
through a tool call:

```harn
const snap = runtime_introspection()
log("provider=" + to_string(snap?.provider))
log("model=" + to_string(snap?.model))
log("harn=" + to_string(snap.harn_version))
```

All fields are `nil` until at least one `llm_call` has run on the
thread; `harn_version` and `harness` are always populated.

## Configuring the harness identity

`current_harness()` reports the value of the `HARN_HARNESS` environment
variable, trimmed. When unset or empty, it falls back to `"harn"` (the
bare CLI). Hosts that embed the runtime set it during process startup:

```shell
HARN_HARNESS=ide-host harn run main.harn
HARN_HARNESS=claude-code harn run main.harn
HARN_HARNESS=cloud harn run main.harn
```

The value is opaque to the runtime — pick whatever string your
observability and prompt code want to read.

## Allowlist

The snapshot exposes only these fields, regardless of what the runtime
knows internally:

```text
harn_version, harness, provider, model, model_alias, family,
tool_format, tier, context_window, runtime_context_window,
capabilities
```

The introspection bundle deliberately does **not** surface:

- API keys or auth tokens
- Environment variables outside `HARN_HARNESS`
- The raw system prompt, conversation messages, or tool definitions
- Host-private paths (cwd, config files, log directories)
- Per-session budgets, rate-limit state, or cost accounting

Hosts that need richer introspection should expose their own bridged
tools rather than extending this surface.

## Introspection vs prompt prose

Prompt prose ("You are running on Claude Opus 4.7 with a 200k context
window") goes stale the moment the operator changes the model alias or
the provider rotates a release. Introspection tools cannot go stale —
they read the resolved facts the runtime is actually using for *this*
turn.

Use the introspection tools when the model needs to:

- Answer identity questions truthfully (defensible product behavior)
- Adapt strategy based on context window (e.g. "I have 100k tokens left,
  I can read the whole file" vs. "I'm near the limit, I'll summarize")
- Pick capability-dependent code paths (vision, tools, structured
  output) without the script having to thread the capability matrix
  into the prompt

Keep prompt prose for stable, persona-level guidance.
