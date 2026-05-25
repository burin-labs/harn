# Workspace Anchor Cache Contract

The session `workspace_anchor` is transient turn context, not durable system
prompt content.

The contract is:

- `SessionState::system_prompt` must stay free of `{{workspace_*}}` and
  `{{project_*}}` template tokens.
- active workspace state is rendered through the canonical `workspace_anchor`
  reminder provider instead
- downstream callers that still assemble `pipeline_input` or system prompt text
  by hand must keep workspace/project fields out of the persisted session
  prompt surface

The canonical reminder body is:

```text
<workspace-anchor>
primary: /Users/me/projects/X
additional_roots:
  - /Users/me/projects/Y (mount_mode: read_only, mounted_at: 2026-05-23T10:15:00Z)
</workspace-anchor>
```

That reminder is injected on `SessionStart` and refreshed after each turn via
the `OnBudgetThreshold` provider pass. Re-anchoring therefore updates the next
turn's reminder payload without rewriting the stored session prompt.

In debug builds, Harn asserts this contract when it records a session system
prompt. If the prompt text contains `{{workspace_*}}` or `{{project_*}}`, the
runtime panics with `HARN-CACHE-001` so the offending caller can move that data
into reminder space instead.
