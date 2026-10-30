- The protocol artifacts now publish a typed body for every `_harn/agentEvent` kind:
  `schemas/agent-event.schema.json`, a record per kind in every binding, a `HarnAgentEventPayload`
  union in Swift, Rust, and TypeScript, and a decodable `HarnAgentEventParams` in Swift. The schema is
  derived from the types the ACP adapter serializes, and the advertised kind list now names every
  kind the adapter emits, including `anchor_changed`, `capability_gap`, `loop_checkpoint`,
  `missing_tool_call_verdict`, and the handler diagnostics.
