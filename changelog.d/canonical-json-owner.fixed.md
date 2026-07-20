- **One canonical-JSON owner for every hash and signature input.** Seven
  near-identical canonicalizers across the VM (`mcp_host`, channels,
  `llm::cache`, lifecycle receipts, merge-captain audit, `project.enrich`, and
  transcript projection) each swallowed serialization failures with
  `unwrap_or_default()`, so an encoding error would have silently contributed an
  *empty* string to a hash instead of surfacing. All seven now route through a
  single `canonical_json` module that delegates to the session-store encoder
  already used to sign session events, leaving the runtime with exactly one
  canonicalizer — VM hashes and session signatures can no longer drift apart.
  Three further digest inputs with the same silent-empty fallback (plan ids,
  LLM mock fixture naming, and agent-observation hashing) were switched to the
  same owner, which also makes them key-order stable. The one genuinely
  fallible boundary — canonicalizing an arbitrary `Serialize` value — now
  returns an error that callers propagate. Encoded bytes are unchanged, so
  existing signed receipts, `input_hash` replay checks, transcript `prefix_hash`
  chains, and on-disk enrichment cache entries all remain valid.
