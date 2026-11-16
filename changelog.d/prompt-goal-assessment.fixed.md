- **ACP prompt results retain the goal assessment from terminal checkpoints (#10639).**
  Hosts receive `goal_outcome` beside the terminal without treating successful
  delivery as task achievement. Rust embedders can use the atomic prompt
  receipt API; the older terminal-only methods remain as deprecated adapters.
