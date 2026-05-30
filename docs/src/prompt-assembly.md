# Prompt assembly

Every system prompt Harn sends to a model is the deterministic reduction of an
ordered list of **fragments**. Host-provided parts (`system_preamble`,
`system_prefix`, `system_context`, `system_prompt_parts`, `system_appendix`,
`system_suffix`), the agent's primary system text, capability-gated tool
guidance, and rendered [system reminders](./system-reminders.md) all flow
through one reducer. There is no parallel string-concatenation path, and there
is no place where an instruction is glued on by hand and silently drifts from
the rest of the prompt.

Because assembly is a single reduction, it is also fully **auditable**: the
runtime can answer "why is this sentence in the prompt?" and "what would the
prompt look like without tool X?" without anyone reverse-engineering a
concatenation.

## Fragments

A fragment is one contributor to the system string:

| field | meaning |
| --- | --- |
| `id` | stable identifier, e.g. `host:system_preamble`, `primary:active_skills`, `tool:todo.guidance` |
| `source` | who contributed it (`host:*`, `primary`, `reminder`, `tool:<name>`) |
| `bucket` | `before` (preamble … primary … reminders) or `after` (appendix/suffix) |
| `requires_tools` | included only when every named tool is in the active tool set |
| `requires_caps` | included only when every named capability flag is set |
| `body` | the already-rendered text; trimmed, and skipped if empty |

The reducer emits all included `before` fragments in declaration order, then all
included `after` fragments, joined by a blank line.

## The primary block is decomposed

The agent's per-turn primary system text is itself a composite — the base system
prompt, MCP advisory context, active skills, the skill catalog, the progress-tool
nudge, and the loop/tool contracts. Rather than glue these into one opaque
`primary` string, [`agent_loop`](./agents.md) hands the assembler each part as its
own fragment through the internal `_system_fragments` channel, so every part is
traced on its own (`primary:system`, `primary:active_skills`,
`primary:loop_contract`, …) and can be gated with `requires_tools` independently.
Joining the fragment bodies with a blank line reproduces the single string the
legacy path produced, so the assembled prompt is unchanged — only its provenance
is finer-grained. `agent_build_turn_system_fragments` is the stdlib helper that
emits the list; `agent_build_turn_system` is the thin wrapper that joins it.

## Tool guidance rides with the tool

The drift-proof way to attach an instruction to a tool is to co-locate it with
the tool definition. Any tool that declares a `guidance` string auto-contributes
a fragment gated on that tool's own presence:

```harn
tool_define(registry, "todo", "Add, update, and complete plan items", {
  parameters: { /* … */ },
  handler: todo_handler,
  guidance: "When working from a plan or task list, always update the TODO tracker after each item.",
})
```

When `todo` is in the active tool set, the instruction appears. Drop the tool and
the instruction vanishes — the two share one source of truth and cannot drift.
This is the canonical answer to "tell the model to use the TODO tracker, but only
when a TODO tool is actually available." No `if`, no hand-maintained list of tool
names in prose.

Guidance is prompt-side metadata: it is rendered into the system prompt but is
never sent to the provider as part of the tool's schema.

## Inspecting the assembled prompt

`prompt_explain(options)` assembles the system prompt from the same options you
would hand [`agent_loop`](./agents.md) and returns the final string plus a
provenance record for every fragment — included or excluded, with the reason and
byte count:

```harn
let explained = prompt_explain({
  system: "You are a pragmatic engineering partner.",
  tools: [
    { name: "todo", description: "Track plan items", guidance: "Always update the TODO tracker." },
    { name: "read", description: "Read a file" },
  ],
})
// explained.system     → the assembled string
// explained.fragments  → [{ id, source, bucket, included, reason, bytes }, …]
// explained.included / explained.excluded → counts
```

Each fragment's `reason` tells you exactly why it is or isn't present — for
example `tool(s) present: todo` for a guidance fragment that made it in, or
`requires tool \`todo\` (not available)` for one gated out. The bundled
`harn demo prompt-guidance` scenario assembles a prompt with and without a tool
and prints the provenance side by side.

Session **reminders** are layered at live call time from the active session, so
`prompt_explain` shows the static system prompt (host parts + primary +
capability-gated tool guidance). See [System reminders](./system-reminders.md)
for the dynamic, turn-boundary layer that composes into the same reducer at call
time.

## Relationship to other primitives

- [System reminders](./system-reminders.md) are the dynamic subset of fragments:
  they carry a lifecycle (TTL, dedupe, preserve-on-compact) and are injected at
  turn boundaries, but they reduce through the same assembler.
- [Prompt templating](./prompt-templating.md) (`.harn.prompt`) renders a
  fragment's `body`; assembly decides whether and where that body appears.
- Capability-adaptive rendering chooses the *wire format* of the assembled
  prompt per model capability; fragment assembly chooses *which content* is
  present.
