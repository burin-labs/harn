- Agent loops now emit a typed `llm_call_start` checkpoint before each
  blocking model call so thin hosts can keep liveness timers and run monitors
  honest during long prompt/model phases.
