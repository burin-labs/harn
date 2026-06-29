OpenAI-compatible providers now strip transcript-only and provider-private
message fields before sending chat completions requests, avoiding strict-provider
rejections of stored reasoning or cache metadata.
