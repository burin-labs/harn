- **High-resolution ASR battery.** `security/fixtures/asr-battery.json` grows
  from 14 fixtures (1–2 per class) to 94 (≥10 *distinct* mechanisms per
  role-confusion class + 11 false-positive controls), so per-class attack-success
  rate resolves a small effect instead of quantizing to 0/1. New `battery.rs`
  invariants make the corpus self-guarding: unique ids, exactly-one `{CANARY}`
  per coupled behavioural payload, no duplicate payloads (trial independence),
  reserved-token presence for special-token attacks, and a ≥10-per-class floor.
