- **Cerebras model catalog tracks the current public endpoint set.**
  `gpt-oss-120b` now uses Cerebras's public discovery pricing, `zai-glm-4.7`
  is cataloged as the current public preview coding/agentic route with native
  tools and `reasoning_effort="none"` support, and the stale Cerebras Llama row
  is marked dedicated-only so clients do not present it as a one-click
  serverless option.
- Harn now models provider-specific `reasoning_effort` value sets so
  Cerebras-hosted `gpt-oss-120b` floors `reasoning_policy: "off"` to the
  endpoint's supported `low` effort instead of sending unsupported `none` or
  `minimal` values, and structured LLM calls accept the same documented routing,
  reasoning, timeout, fast-mode, and prompt-assembly option keys as `llm_call`.
