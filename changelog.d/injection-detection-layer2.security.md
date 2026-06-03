- **On-device injection detection (`mode = "local-ml"`).** Untrusted content is now scored by a
  pluggable injection classifier and the verdict (`model`, `score`, `flagged`) is recorded on its
  taint record, so the approval UI and audit trail can show *why* a span looks risky. The built-in
  `heuristic-v1` classifier is always available, dependency-free, and precision-first (instruction-
  override phrasing, concealment directives, hidden/bidi unicode); a downloadable neural model
  (`harn-guard`) can supersede it via `register_injection_classifier` without the default binary ever
  linking a model runtime — no bundled weights, paid API, or network. A flagged verdict tightens the
  trifecta gate: in addition to exfil/destroy/secret-read, a flagged injection plus a workspace-
  mutating tool (a file write) is now gated too, catching injection→write attacks the bare trifecta
  misses. Detection never weakens the gate. Configure via `[security]` (`detect_injection`,
  `guard_threshold_percent`) or `std/security::local_ml()`.
