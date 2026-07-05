- Added `std/agent/hypothesis`: a keyed store of human/agent hypotheses (id,
  statement, a mutable free-form `status`, and provenance) layered on
  `std/session-store`. `hypothesis_open` / `hypothesis_set_status` /
  `hypothesis_get` / `hypothesis_list` (with an optional status filter) /
  `hypothesis_delete` project the append-only stream; events carry the special
  `{kind: "hypothesis"}` tag.
- Added `memory_reminders(namespace, options?)` to `std/memory`: a
  deterministic callable returning the active records flagged to auto-surface
  as reminders (default flag `auto_surface`, overridable via `options.flag`) —
  a filtered enumeration safe to call every turn.
