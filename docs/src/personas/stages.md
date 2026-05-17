# Per-stage tool scoping

A persona that walks through `research → plan → edit → verify` usually
runs the entire run under one ambient `CapabilityPolicy`. The model is
trusted by prompt convention to only call read-only tools during
`research`. That trust is one prompt injection away from breaking.

Persona stage declarations close the gap: each stage names a `@step` and
the tool / side-effect surface enforced for the duration of that step.
The runtime pushes a `CapabilityPolicy` on entry and pops it on exit, so
a tool call that escapes the declared surface comes back as a
`permission_denied` receipt without ever reaching the handler.

## Declaring stages

Stages are part of the persona manifest. They can live in `harn.toml`:

```toml
[[personas]]
name = "merge_captain"
tools = ["github", "ci", "linear", "notion", "slack", "mcp"]
# ...
stages = [
  { name = "classify", allowed_tools = ["github", "ci"], side_effect_level = "read_only" },
  { name = "plan_repair", allowed_tools = ["github", "ci", "linear", "notion"], side_effect_level = "read_only" },
  { name = "apply_repair", allowed_tools = ["github", "ci"], side_effect_level = "process_exec", on_exit = { on_complete = "classify", on_failure = "plan_repair" } },
]
```

or directly on the `@persona` attribute in a Harn workflow:

```harn
@persona(
  name: "scoped_persona",
  tools: [github, ci],
  stages: [
    {name: "research", allowed_tools: ["github"], side_effect_level: "read_only"},
    {name: "act", allowed_tools: ["github", "ci"], side_effect_level: "process_exec"},
  ],
)
fn scoped_persona(ctx) {
  research(ctx)
  act(ctx)
}

@step(name: "research") fn research(ctx) { return ctx }
@step(name: "act") fn act(ctx) { return ctx }
```

Each stage's `name` must match a `@step(name: "...")` declaration in the
same persona. The matched step's frame is what activates and deactivates
the stage policy.

## Fields

| Field               | Type                | Meaning                                                                                                    |
| ------------------- | ------------------- | ---------------------------------------------------------------------------------------------------------- |
| `name`              | string              | Matches a `@step` declaration. Required.                                                                   |
| `allowed_tools`     | `string[]?`         | Tool allowlist for the stage. Omit to inherit the persona's `tools`. `[]` denies every tool.               |
| `side_effect_level` | string?             | One of `none` / `read_only` / `workspace_write` / `process_exec` / `network`. Tightens the ambient ceiling. |
| `max_iterations`    | u32?                | Optional agent-loop iteration cap surfaced to downstream consumers.                                        |
| `on_exit`           | `{on_complete?, on_failure?}` | Transition hints recorded with the stage receipt. The runtime does not auto-route — the workflow does. |

## Validation

Persona load (`harn persona list`, `harn check`) rejects manifests where:

- A stage's `allowed_tools` references a tool that isn't in the persona's
  top-level `tools` list, or isn't known to the host tool catalog.
- `side_effect_level` is not one of the canonical names above.
- `on_exit.on_complete` / `on_exit.on_failure` references a stage that
  the persona doesn't declare.
- Two stages share a `name`.

## Runtime semantics

Stage policies are pushed onto the same `EXECUTION_POLICY_STACK` that
`with_execution_policy(...)` uses. They intersect with whatever ambient
policy was already active — a stage can tighten the surface but never
loosen it. When a step frame unwinds (success, error, or escape via
`on_exit`), the runtime pops the stage policy, mirroring the existing
RAII guard pattern used by ACP modes.

A stage with `allowed_tools = [...]` and no `side_effect_level` leaves
the ambient side-effect ceiling intact; combine both fields to scope a
read-only research stage against a workspace-mutating persona.

If `stages` is empty (or omitted), behavior is identical to a persona
without stage declarations — the ambient policy alone governs every
tool call.

## Per-dispatch scoping outside a persona

For callers that don't run under a persona — ad-hoc agents, one-off
`agent_loop` invocations, or test harnesses — the same scoping is
available as a tool-caller middleware:
[`with_scoped_executor(...)`](../stdlib/tool-middleware.md). It pushes
a `CapabilityPolicy` for the duration of a single dispatch (intersected
with whatever ambient policy is active) and decorates the result with
`audit.scope = {stage, allowed_tools, ...}`. Out-of-scope tool names
short-circuit with `status: "scope_violation"` so callers can route on
the typed receipt.
