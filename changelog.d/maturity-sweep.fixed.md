- **Tool-call parsing no longer shreds calls whose arguments contain the
  protocol's own tags.** A literal `</tool_call>`, a `<<TAG … TAG` heredoc, or a
  bash `<<EOF` inside a quoted string argument is now treated as content, not as
  the structural close, across the buffered parser, the streaming detector, and
  the wrapper-stripper. Two `<tool_call>` blocks in one turn also get
  turn-unique ids instead of both colliding on `tc_0`.
- **`base64`/`base64url`/`base32`/`hex` encoding and the `sha2`/`md5` hashes are
  lossless for `bytes` input.** They previously routed `bytes` through the
  display form, silently truncating binary payloads at 32 bytes; they now accept
  `string | bytes` and hash/encode the raw bytes.
- **Durable `step.run` replay is no longer quadratic.** Replay detection uses an
  indexed idempotency lookup instead of rescanning the whole step topic on every
  step, so a K-step workflow stops doing O(K²) work.
- **Anthropic structured-output requests stop silently discarding a
  caller-supplied `tool_choice`/tool set.** Structured output still wins, but it
  warns once and preserves the caller's tools instead of dropping them with no
  signal.
- **Import errors name the directory a relative import was resolved against**, so
  it's clear whether resolution was relative to the importing file or the CWD.
