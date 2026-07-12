# Typed host-event injection

Status: Harn extension

Harn advertises `session/inject_host_event` under
`agentCapabilities.session.injectHostEvent`. The request carries a session ID
and the same typed event contract used by the in-process runtime:

```json
{
  "sessionId": "session-1",
  "event": {
    "kind": "host_tool_result",
    "delivery": "turn_boundary",
    "payload": {
      "tool_name": "host.search",
      "raw_input": { "query": "typed injection" },
      "raw_output": { "matches": 3 }
    },
    "provenance": {
      "initiator": "user",
      "source": "ide",
      "ts_ms": 1773298800000
    }
  }
}
```

Supported kinds are `host_tool_result` and `host_attachment`. Supported
delivery seams are `immediate`, `turn_boundary`, and
`after_next_tool_call`. Harn validates, sanitizes, records, queues, and renders
the event; hosts cannot supply rendered prompt material. Unknown fields are
rejected so misspelled provenance or delivery policy cannot silently degrade.

Successful responses contain the durable `injection_id`, monotonic `sequence`,
resolved `delivery`, and `status`. Unknown sessions use JSON-RPC error `-32004`;
invalid request or payload contracts use `-32602`.

The generated TypeScript, Swift, Python, Go, and Rust artifacts publish this
method vocabulary. Typed request structures are included in every generated
language except the dependency-free Rust vocabulary module, where embedders use
`harn_serve::adapters::acp::AcpSessionInjectHostEventParams` directly.
