# Hooks

Harn exposes three concentric hook surfaces. Each surface fires
synchronously on the agent-loop thread; closure handlers run inside the
same VM context as the surrounding turn, so they see the live transcript
and can call any builtin. Hook invocations are recorded on the active
session transcript under `hook_call`, `hook_returned`, and `hook_vetoed`
event kinds, so replay tooling reproduces the same control flow byte for
byte.

## Tool lifecycle hooks (`register_tool_hook`)

`PreToolUse` and `PostToolUse` fire around each tool dispatch.
`PreToolUse` returns `{deny: reason}` to refuse the call or
`{args: replacement}` to rewrite the arguments; `PostToolUse` returns a
string or `{result: replacement}` to rewrite the result.

```harn
register_tool_hook({pattern: "exec_*", deny: "exec is gated"})
register_tool_hook({pattern: "*", max_output: 4000})
```

## Persona / step lifecycle hooks (`register_persona_hook`, `register_step_hook`)

Persona-scoped hooks observe `PreStep`, `PostStep`,
`OnApprovalRequested`, `OnHandoffEmitted`, `OnPersonaPaused`,
`OnPersonaResumed`, and `OnBudgetThreshold(pct)`. Step hooks pin a
matching pair (`persona_pattern`, `step_name`) and accept the same
events; both can deny via `{deny: reason}` or rewrite via `{args}` /
`{output}`.

## Session lifecycle hooks (`register_session_hook`)

Session-level hooks fire from the whole-session turn loop. They are
the primary plugin surface for hosts and package authors who want to
observe or veto the surrounding session rather than each individual
tool call.

| Event | When it fires | Veto |
|---|---|---|
| `session_start` | After the session record is open, before the loop starts | Advisory |
| `session_end` | After the loop exits, before native session-end hooks fire | Advisory |
| `user_prompt_submit` | Before the agent sees the user's prompt | `{block: true, reason}` returns a `blocked` result with `stop_reason: user_prompt_submit_blocked` |
| `pre_compact` | Before transcript autocompaction runs | `{block: true}` skips this compaction pass |
| `post_compact` | After autocompaction completes and replaces messages | Advisory |
| `post_turn` | After a model/tool turn is recorded, before post-turn control logic decides whether to continue | Advisory |
| `permission_asked` | When the dynamic permission policy needs to escalate | `{decision: "allow"\|"deny", reason}` short-circuits the policy |
| `permission_replied` | After the dynamic permission policy decides | Advisory |
| `file_edited` | After `write_file` / `append_file` / `write_file_bytes` / `notify_file_edited` queues an edit. Drained at each agent-loop turn boundary | Advisory |
| `session_error` | Before `session_end` when the loop ended with an error status or terminal error | Advisory |
| `session_idle` | Each time the daemon-mode agent loop enters its `wake_interval_ms` wait between turns | Advisory |
| `pre_finish` | Just before the pipeline's `on_finish` callback runs (or before pipeline return when no callback is registered) | Advisory |
| `on_unsettled_detected` | Between `pre_finish` and `on_finish`, but only when `harness.unsettled_state()` is non-empty | Advisory |
| `post_finish` | After the pipeline's `on_finish` callback returns, just before the pipeline value is yielded to the host | Advisory |

### Return-value protocol

| Return | Meaning |
|---|---|
| `nil` or `true` | Allow / advisory acknowledgment |
| `false` | Veto (same as `{block: true}`) |
| `{block: true, reason: ...}` | Veto |
| `{decision: "allow"\|"deny"\|"ask", reason?: ...}` | Permission short-circuit (only honoured for `permission_asked`) |

Any other return shape raises a runtime error so misuse fails loudly.

### Example: veto secrets

```harn
register_session_hook("user_prompt_submit", { event ->
  let prompt = to_string(event?.prompt ?? "")
  if prompt.contains("secret") {
    return {block: true, reason: "policy violation: secret in prompt"}
  }
  return nil
})
```

### Example: short-circuit a permission decision

```harn
register_session_hook("permission_asked", { event ->
  if to_string(event?.tool?.name ?? "") == "exec_root" {
    return {decision: "deny", reason: "exec_root never runs unattended"}
  }
  return nil
})
```

### Example: re-run a linter after each file edit

```harn
register_session_hook("file_edited", { event ->
  let path = to_string(event?.path ?? "")
  if path.ends_with(".rs") {
    log("re-running clippy after edit to " + path)
  }
  return nil
})
```

For non-blocking context refresh, librarian, and crystallization jobs, use the
receipt contract in [Context maintenance hooks](../context-maintenance-hooks.md)
instead of doing slow work inside the hook closure.

## `[[hooks]]` package manifest entries

Declarative entries in `harn.toml` register VM hooks at package load
time. The event name maps 1:1 to the runtime enum (`PreToolUse`,
`SessionStart`, `UserPromptSubmit`, etc.):

```toml
[[hooks]]
event = "UserPromptSubmit"
pattern = "*"
handler = "policy::reject_secrets"
```

The handler must resolve to an exported `pub fn` in the package's
namespaced exports. The harness wires the closure via
`register_vm_hook` and the resulting hook participates in the same
veto / tape capture semantics as a programmatic registration.

## Tape capture and replay

Every `register_session_hook` invocation writes three transcript
events:

- `hook_call` — payload + handler name, before the closure runs.
- `hook_returned` — handler name + parsed control flow, after it runs.
- `hook_vetoed` — emitted only when control flow is `block` or a
  permission `decision`.

Because the events are first-class transcript entries, replay tools
that re-execute a recorded session see the same hooks fire in the
same order with the same payloads, even if the closures are no longer
registered. This is the basis for the replay-fidelity guarantee
used by transcript, trigger, and orchestrator replay tooling.
