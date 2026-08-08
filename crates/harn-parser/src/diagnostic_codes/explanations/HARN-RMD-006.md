# HARN-RMD-006

A reminder provider closure returned a value that could not be parsed as a
`ReminderSpec`. Provider closures may return `nil`, a reminder spec, an effect
such as `{reminder: {...}}`, or a list of those effects.

Return a dict with a non-empty `body` and only supported reminder fields.

Example fix:

```harn
register_reminder_provider({
  id: "custom",
  subscribes_to: ["session_idle"],
  evaluate: { _ctx ->
    return {reminder: {body: "Re-check current session state.", ttl_turns: 1}}
  },
})
```
