# Hooks

Harn exposes three concentric hook surfaces. Each surface fires
synchronously on the agent-loop thread. Runtime hook handlers run inside the
same VM context as the surrounding turn and receive the root `Harness` plus
the event. Importing a module grants no authority, and reusable helpers should
accept only the coherent sub-handle they need. Hook invocations are recorded on
the active
session transcript under `hook_call`, `hook_returned`, and `hook_vetoed`
event kinds, so replay tooling reproduces the same control flow byte for
byte.

## Tool lifecycle hooks (`register_tool_hook`)

`PreToolUse` and `PostToolUse` fire around each tool dispatch.
`PreToolUse` returns `{deny: reason}` to refuse the call or
`{args: replacement}` to rewrite the arguments; `PostToolUse` returns a
string or `{result: replacement}` to rewrite the result. A hook that drops
source bytes returns
`{result: replacement, truncated: true, dropped_bytes: N}` so later hooks
cannot erase the truncation signal by appending content. For custom logic,
pass `pre` and/or `post` closures in the config table.

```harn
register_tool_hook({pattern: "exec_*", deny: "exec is gated"})
register_tool_hook({pattern: "*", max_output: 4000})
register_tool_hook(
  {
    pattern: "fetch_*",
    pre: { _hook_harness, event ->
      return {reminder: {body: "Network fetch is about to run", tags: ["tool"]}}
    },
  },
)
```

## Persona / step lifecycle hooks (`register_persona_hook`, `register_step_hook`)

Persona-scoped hooks observe `PreStep`, `PostStep`,
`OnApprovalRequested`, `OnHandoffEmitted`, `OnPersonaPaused`,
`OnPersonaResumed`, and `OnBudgetThreshold(pct)`. Step hooks pin a
matching pair (`persona_pattern`, `step_name`) and accept the same
events; both can deny via `{deny: reason}` or rewrite via `{args}` /
`{output}`.

Hook closures may also return a reminder effect for the active session
transcript. The reminder spec uses the same keys as
`transcript.inject_reminder`: `body`, `tags`, `dedupe_key`,
`ttl_turns`, `preserve_on_compact`, `propagate`, and `role_hint`.

```harn,ignore
register_step_hook("merge_*", "audit", "PreStep", { _hook_harness, ctx ->
  return {
    reminder: {body: "Audit step is running", tags: ["audit"]},
    then: {args: ctx.step.args},
  }
}
```

## Session lifecycle hooks (`harness.agent.register_session_hook`)

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
| `file_edited` | After a standard filesystem mutation or `notify_file_edited` queues an edit. Drained at each agent-loop turn boundary | Advisory |
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
| `{reminder: {...}, then?: ...}` | Inject a `system_reminder`, then apply the optional inner control/action |
| `{body: "...", tags?: [...], dedupe_key?: ...}` | Inject a reminder and otherwise allow/pass |
| `[{reminder: {...}}, ...]` | Session hook effect list; reminders are injected in order and deduped by `dedupe_key` |

Any other return shape raises a runtime error so misuse fails loudly.

### Example: veto secrets

```harn
fn reject_secret_prompt(stdio: HarnessStdio, event) {
  const prompt = to_string(event?.prompt ?? "")
  if prompt.contains("secret") {
    stdio.log("blocked a prompt containing a secret")
    return {block: true, reason: "policy violation: secret in prompt"}
  }
  return nil
}

pipeline main(harness: Harness) {
  harness.agent.register_session_hook("user_prompt_submit", { hook_harness, event ->
    return reject_secret_prompt(hook_harness.stdio, event)
  })
}
```

### Example: short-circuit a permission decision

```harn
pipeline main(harness: Harness) {
  harness.agent.register_session_hook("permission_asked", { _hook_harness, event ->
    if to_string(event?.tool?.name ?? "") == "exec_root" {
      return {decision: "deny", reason: "exec_root never runs unattended"}
    }
    return nil
  })
}
```

### Example: re-run a linter after each file edit

```harn
pipeline main(harness: Harness) {
  harness.agent.register_session_hook("file_edited", { hook_harness, event ->
    const path = to_string(event?.path ?? "")
    if path.ends_with(".rs") {
      hook_harness.stdio.log("re-running clippy after edit to " + path)
    }
    return nil
  })
}
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

The handler must resolve to an exported `pub fn` in the package's namespaced
exports with the signature `fn(harness: Harness, event)`. A manifest handler is
a runtime entry boundary, so it receives root authority; reusable helpers
should accept only the coherent sub-handle they need. The runtime wires the
closure through the same hook registry and the resulting hook participates in
the same veto and tape-capture semantics as a programmatic registration.

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
