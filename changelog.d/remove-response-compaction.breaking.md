- **Removed `std/agent/response_compaction`, `std/agent/compaction_classify`, and the `classify` compaction strategy.**
  Nothing in the runtime or the shipped pipelines used them. The `classify` strategy and its `classify` option
  (`compaction.classify`, `transcript_compact`, `transcript_auto_compact`, `agent_session_compact`, compaction
  policies) are gone, as are the `classification` field on compaction receipts, events and ACP `_meta.harn`, and the
  "via classified compaction" recap header. `compact_strategy: "classify"` is now an unknown-strategy error, and an
  `auto_compact` strategy label of `classify` falls back to observation masking like any other unrecognized label.

  ## Migration

  Use `observation_mask` or `llm` compaction. Callers that stored large values through `response_compaction` should
  keep the exact value in `std/cache` themselves and summarize it with an ordinary `harness.llm` call. Rust consumers
  constructing `CompactionReceipt`, `CompactionEventRecord`, `AutoCompactConfig` or `CompactionPolicyDeclaration` must
  drop the `classification` field; exhaustive matches on `CompactStrategy` or `PolicyStrategy` lose the `Classify`
  variant. Vendored protocol bindings generated from `spec/protocol-artifacts` must be regenerated:
  `transcript_compacted` session updates no longer carry `_meta.harn.classification`, and bindings that required it
  reject the new payload.
