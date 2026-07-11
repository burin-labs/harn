- Preserve Bedrock Converse tool-call history: assistant `tool_use` turns and
  structured tool results now render as `toolUse`/`toolResult` content blocks
  instead of being dropped as empty text, so agentic tool loops on Bedrock no
  longer 400 with "messages must alternate".
