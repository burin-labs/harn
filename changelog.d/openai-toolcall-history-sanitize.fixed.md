Sanitize nested OpenAI-compatible assistant `tool_calls` history before provider
dispatch so strict OpenRouter/Fireworks routes do not receive storage-only or
telemetry fields.
