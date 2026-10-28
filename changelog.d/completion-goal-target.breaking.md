Session goal metadata now includes the optional requested edit target used to
bind behavior-preserving verification evidence to the active goal.

Migration: Rust callers constructing `SessionGoalMetadata` with an exhaustive
struct literal must supply `target: None`, or `target: Some(identity)` when the
host has declared the requested target. Existing serialized goals without the
field continue to deserialize without a target.
