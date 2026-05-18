# HARN-RMD-002

**Variant:** `Code::ReminderInvalidShape`

The reminder payload shape is invalid.

`session/remind` expects a typed reminder object, not a user-message payload.
Provide a non-empty `body`, use string `tags`, a positive integer `ttl_turns`
when present, boolean `preserve_on_compact`, and one of the documented
`propagate` and `role_hint` values. Put host-specific extension fields under
`_meta`.
