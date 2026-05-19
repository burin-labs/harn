# HARN-RMD-005

**Variant:** `Code::ReminderUnknownPropagate`

A reminder specified an unsupported `propagate` value. Reminder propagation must
be one of `all`, `session`, or `none`.

Use `all` for reminders that should follow child agents, `session` for the
current session only, and `none` for reminders that should never propagate.

Example fix:

```harn
transcript.inject_reminder(transcript(), {
  body: "Check the task ledger before continuing.",
  propagate: "session",
})
```
