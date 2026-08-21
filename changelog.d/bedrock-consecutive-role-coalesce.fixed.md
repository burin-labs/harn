- **Bedrock Converse no longer rejects a transcript with consecutive same-role
  turns.** A parallel tool call produces one tool-result turn per call and each
  maps to a `user` turn, so any fan-out wider than one returned a 400 from
  Converse, which requires alternating roles and specifies parallel results as
  several `toolResult` blocks inside a single `user` message. The adapter now
  folds each run of consecutive same-role turns into one turn carrying the
  concatenated content blocks, which also covers an assistant prefill landing
  behind a trailing assistant turn. Content, ordering, and tool-use ids are
  unchanged; only the turn boundaries move.
