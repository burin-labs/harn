- **`std/agent/pins`: compaction pins as data.** A new module generalizes
  Burin's structural working-set into a typed pin taxonomy — `pin(kind, content,
  opts?)` / `unpin(pins, id)` over the kinds `artifact_ref`, `constraint`,
  `decision`, `goal`, and `no_compact`. Pins survive compaction by construction:
  `pin_reminder(pin)` lowers to a `preserve_on_compact` system reminder and
  `pin_compaction_policy(pins)` emits `compaction_policy` preserve directives.
  The same pins double as reachability-GC roots — `with_pin_roots(opts, pins)`
  feeds pin content into the existing `reachability_gc` projection `roots` config
  so any referenced stale tool result is kept, not reclaimed.
  `recognize_no_compact(text)` is a documented ingestion adapter for Burin's
  literal `[no-compact]` heading marker. Agent presets can carry a default
  `pin_policy` pack row (the long-running captains do). Additive stdlib only — no
  new host surface.
