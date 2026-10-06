- **`harn_vm::agent_events::AgentEvent::ToolCall` has a new `intent: Option<String>` field.** It carries
  the turn's declared purpose to the tool-call start event. Serialized events are unchanged when it is
  `None`, and events recorded before this field decode with `None`. Rust code that constructs the variant
  must set the field; code that matches it with `..` is unaffected.

  Migration: add `intent: None` (or the normalized purpose) where you build the variant.

  ```rust
  // before
  AgentEvent::ToolCall { session_id, tool_call_id, tool_name, kind, status, raw_input, parsing, audit }
  // after
  AgentEvent::ToolCall { session_id, tool_call_id, tool_name, kind, status, raw_input, parsing, audit, intent: None }
  ```
