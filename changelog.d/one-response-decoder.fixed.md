Streamed Anthropic and OpenAI-compatible responses now decode exactly like the
same response returned without streaming. The stream is folded back into the
provider's own response body and read by the one response parser, so a feature
can no longer reach one path and miss the other. Fixes found by the new parity
check:

- Parallel tool calls from an OpenAI-compatible stream keep the provider's
  order. They were returned in hash-map order.
- A streamed inline `<think>` span is recorded as a private reasoning block, and
  streamed server tool-search no longer splits the answer text into two blocks.
- Unparseable tool arguments on a length-truncated turn become `{}` on both
  paths, and empty arguments are a no-arguments call on both paths.
- An unstreamed Anthropic turn that bills output but commits neither text nor a
  tool call is now rejected as a billed no-op, as the stream already did.
- An unsigned Anthropic `thinking` block is private reasoning on both paths
  instead of an unreplayable signed block.
- A stream that delivers no content now fails as an empty generation even when
  it reported no output tokens, matching the unstreamed path.
- A streamed Anthropic-wire result reports the configured provider name instead
  of always `anthropic`.
