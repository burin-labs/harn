- **Verification results retain their declared field types (#9347).**
  `std/verification` re-exports its owning types instead of erasing command
  receipts. Diagnostic deltas, ladder entries, HUD models, and freshness
  results now have explicit field contracts. Record decoders preserve nominal
  containers, legacy scalar coercions, and extension fields.

  Migration: read `VerificationStartedCheck.started.status` as a string,
  replacing assignments that relied on its erased type. Guard a HUD model's
  optional `last_check`, `next_check`, and `full_check` before accessing their
  fields. Use optional access or a nil guard for indexed ladder entries, and
  decode their `command` and other opaque metadata with `schema_parse` or
  `schema_expect` before reading nested fields.
