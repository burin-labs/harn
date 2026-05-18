# HARN-RMD-004

**Variant:** `Code::ReminderInfiniteDiscardable`

A reminder literal sets `preserve_on_compact: false` while leaving
`ttl_turns` unset or `nil`. That reminder can live forever during normal
turns, but it is allowed to disappear at the next transcript compaction.

Set a finite `ttl_turns` for short-lived nudges, or set
`preserve_on_compact: true` when the reminder must survive compaction.
