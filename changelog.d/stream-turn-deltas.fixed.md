- The agent loop's main request streams its visible text to an ACP host as
  `call_progress` deltas while the model writes, instead of arriving only as one
  message at the end. A `harness.llm.call` that reaches `llm_call` without the
  bridge-registered builtin now uses the host bridge the ACP server installed.
