# HARN-RMD-007

An `agent_loop` enables more than eight distinct reminder providers. Many
providers can inject overlapping ambient context and increase prompt size.

Disable providers that are not useful for the loop, or split the loop into
smaller stages with different reminder settings.

Example fix:

```harn
agent_loop(task, nil, {
  reminders: {providers: ["token_pressure", "idle_nudge"]},
})
```
