- The agent loop now recovers from a provider `context_overflow` error instead
  of dying. When a provider rejects a turn because the assembled prompt exceeds
  the model's context window, the loop emergency-compacts the transcript
  (deterministic observation masking) and retries, bounded so a pathological
  irreducible prompt can't loop forever; only a still-overflowing,
  no-longer-shrinkable transcript becomes a terminal error.
- Added the Fireworks-served `accounts/fireworks/models/gpt-oss-120b` route to
  the model catalog with its real 131072-token context window. Without a catalog
  row the route had no window, so auto-compaction had no budget to enforce and
  the prompt could grow until Fireworks returned HTTP 400 `[context_overflow]`.
