- **Gemini/Vertex thinking tokens + Vertex/Bedrock cache tokens in usage
  accounting.** The Gemini and Vertex adapters now fold
  `usageMetadata.thoughtsTokenCount` into `output_tokens`, so thinking-enabled
  models no longer under-report billed output (and cost). Vertex now also reads
  `cachedContentTokenCount` into `cache_read_tokens` (previously dropped), and
  the Bedrock Converse adapter surfaces `cacheReadInputTokens` /
  `cacheWriteInputTokens` as `cache_read_tokens` / `cache_write_tokens`.
