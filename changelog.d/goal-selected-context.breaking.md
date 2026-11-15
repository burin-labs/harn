Goal reminders can omit an objective already present verbatim in a selected
user message, while retaining full durable text and distinct criteria. Typed
goal projections require `lifecycle: managed | context` and
`objective_covered_body`. Context projections render saved goals without
participating in managed retarget or completion policy. Goal rendering stays
with the canonical formatter; no source anchors or message-origin fields are
added.

Migration: direct `GoalPinProjection` constructors must add `lifecycle` and
`objective_covered_body`. Use `"managed"` for an active goal that participates
in retarget and completion policy, and `"context"` for saved advisory context.
For example, `{spec: goal}` becomes
`{spec: goal, lifecycle: "managed", objective_covered_body: full_body}`;
using the full reminder body preserves its text without shortening. Prefer
the projection returned by `pin_reminder`, which prepares both renderings
through the canonical formatter. Apply any custom prefix or length bound to
both reminder renderings.
