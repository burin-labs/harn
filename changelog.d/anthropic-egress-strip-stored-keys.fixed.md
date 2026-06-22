- Anthropic provider: strip storage-only message keys from outgoing Messages
  API requests. A durable assistant turn persists a top-level `reasoning` key;
  echoing it back into `messages[]` triggered a non-retryable HTTP 400
  (`messages.N.reasoning: Extra inputs are not permitted`) that bricked
  thinking-enabled direct-Anthropic runs. Only canonical message-level fields
  (`role`, `content`, `cache_control`) survive the egress boundary; the
  persisted transcript shape is unchanged, so replay and other providers'
  adapters still see `message.reasoning`.
