# HARN-RMD-008

A hook handler returned a reminder effect from a lifecycle event that cannot
inject reminders. Worker lifecycle events are observational and must not mutate
the active transcript with reminder effects.

Move the reminder to a session, tool, step, or persona hook that runs at a
turn-boundary mutation point.

Example fix:

```harn
register_session_hook("post_turn", { _event ->
  return {reminder: {body: "Review worker progress before continuing."}}
})
```
