- **LLM retry-after handling now parses provider messages with trailing
  punctuation.** Rate-limit retries such as Cerebras
  `(retry-after: 60))` now honor the full provider delay instead of falling
  back to short exponential retries that can immediately hit another 429.
