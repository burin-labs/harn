# HARN-RMD-001

**Variant:** `Code::ReminderUnknownOption` (reminder lifecycle option key is not recognized)

Reminder lifecycle option tables reject unknown keys so reminder shape stays stable across
transcript transforms, hooks, and bridge integrations.

Use only the documented `transcript.inject_reminder` keys: `body`, `tags`, `dedupe_key`,
`ttl_turns`, `preserve_on_compact`, `propagate`, and `role_hint`.

For `transcript.clear_reminders`, use at least one selector from `id`, `tag`, or `dedupe_key`.
