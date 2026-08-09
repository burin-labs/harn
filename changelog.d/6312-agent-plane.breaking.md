- **The agent plane now has one loop entrypoint and one typed contract (#6312).**
  `AgentSpec` replaces `AgentLoopOptions`; `agent_loop` returns `AgentResult`
  with the producer-owned terminal outcome. The redundant `agent_turn`,
  `agent_llm_turn`, `agent_chat_loop`, `std/agent/chat`, and
  `std/agent/completions` surfaces are removed. One-shot model requests use
  `harness.llm`, while hosts own chat and editor-completion presentation. The
  persisted-artifact reader type is renamed to `AgentResultArtifact`, leaving
  `AgentResult` as the single live loop envelope.
