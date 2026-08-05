# Handoff policy overrides

Persona handoffs can carry a `policy_override` field on the typed handoff
payload. The value is a normal `CapabilityPolicy` dict:

```harn,ignore
handoff_dispatch({
  kind: "merge_receipt",
  source_persona: "merge_captain",
  target_persona_or_human: {kind: "persona", id: "review_captain"},
  task: "Review the merge receipt",
  reason: "CI requires a focused review before landing",
  policy_override: {
    tools: ["read_note", "comment_on_pr"],
    side_effect_level: "workspace_write",
  },
  reminder_propagation: [],
})
```

`handoff(...)`, `handoff_routed(...)`, and `with_handoff_artifact(...)` preserve
`policy_override` on the typed envelope as `handoff.policy_override`. When
`handoff_dispatch(...)` invokes a target persona/sub-agent dispatcher, it pushes
that policy as a replacement execution-policy frame for the duration of the
target invocation. Replacement matters: the override is the active target policy
for that handoff run, rather than a merge with the target's previous session
policy.

The same handoff envelope can carry `reminder_propagation`, a filtered list of
pending system reminders inherited by the target. If the field is omitted,
`handoff(...)` fills it from the active parent session: `propagate: "all"`
reminders are forwarded through every descendant, `propagate: "session"`
reminders reach direct child sub-agents only, and `propagate: "none"` reminders
stay local. Inherited copies use `source: "inherited"` and carry
`originating_agent_id` for audit.

Dispatch also attaches `audit.scope.source_handoff = handoff_id` to the queued
payload and dispatch receipt. Target-side receipts and tool audits should carry
that scope forward so reviewers can trace every constrained action back to the
handoff that authorized it.

Use this field when the source persona needs to delegate a sharply bounded piece
of work: for example, handing review to another persona with read-only project
tools, or handing release-note edits to a persona that may write only specific
workspace files. Keep the override minimal and task-specific.
