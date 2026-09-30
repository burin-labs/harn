- Retargeting a session now updates `with_goal` prompts and `goal_judge`
  prompts, retires goal pins frozen under the previous objective, and records
  retired typed criteria for replay. Reusing a `goal_pin` through `agent_pin`
  projects the current objective instead of restoring the abandoned goal.

  Typed goal pins add the optional `goal_pin` field to Rust `SystemReminder`.

  Migration: Rust code that constructs `SystemReminder { ... }` directly must
  add `goal_pin: None` for ordinary reminders. `SystemReminder::new(...)`
  already initializes the field. Existing JSON reminders can omit it.
