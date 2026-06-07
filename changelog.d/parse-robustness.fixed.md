- **Three parse-robustness fixes for the agent tool-call path.** The
  native-JSON salvage path no longer panics on multi-byte UTF-8
  (emoji/accents/CJK) in trailing prose after a `[{"id":...}]` array — it now
  parses the first JSON value with a boundary-safe forward
  `serde_json::Deserializer` instead of an O(n^2) backward byte scan that could
  slice mid-codepoint and abort the turn. The tagged-protocol fence-parity
  check no longer treats an earlier *unbalanced* ` ``` ` as fencing a later
  legitimate `<tool_call>` block (which dropped the call and injected a spurious
  protocol violation); an open fence only encloses a tag when a matching close
  follows. And `agent_loop` now injects `parse_guidance` on partial-success
  turns (some calls parsed, one malformed) flagged `has_partial_success` with
  the dispatched-call count, so the model gets a signal to re-emit the dropped
  call instead of zero feedback — while the no-progress stall suppression stays
  gated on full parse drops only.
