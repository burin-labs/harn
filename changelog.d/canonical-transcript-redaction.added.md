- **Canonical manifest-producing redaction for whole transcripts and run
  records.** `RedactionPolicy` gains `redact_json_manifest` — a single walk that
  scrubs an arbitrary JSON structure (a transcript, a serialized `RunRecord`, a
  session bundle) in place while returning an auditable `RedactionEntry` manifest
  for every value it touched — and `find_unredacted_secret`, the symmetric
  share/ingest gate that refuses a payload still carrying a high-confidence
  secret. These were previously private helpers inside `session_bundle`; they now
  live in `harn_vm::redact` so every export surface (session bundles today;
  portal transcript download, TUI export, and harn-cloud tape ingest next) calls
  one engine instead of reimplementing the walk and drifting from the
  leaf-scrubbing policy. `session_bundle` now consumes the canonical functions
  with identical behavior; `RedactionEntry` moved to `harn_vm::redact`.
