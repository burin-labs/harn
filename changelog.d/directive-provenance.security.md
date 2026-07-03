- **Origin-authenticated cross-agent directives (default-OFF `authenticate_directives`).**
  Defends the measured `cross_agent_poison` weak class — a forged
  `Orchestrator directive:` / `Coordinator override:` planted inside a subagent's
  untrusted result that the model obeys as if it were a real orchestration
  directive (arXiv:2504.16902 / arXiv:2506.23260). The new
  `security::provenance` module stamps a legitimate directive with a
  process-scoped HMAC over `(emitter, body)` — reusing the same per-process
  signing pattern the channel journal already uses, not a new PKI — and
  authenticates directive-looking spans on the read/ingest path: a marker with a
  stamp that verifies is `Authenticated` and passes through; a marker with no /
  invalid stamp is `Forged` and is classified `TrustLevel::Untrusted`, flowing
  into the existing `TaintRecord` ledger and lethal-trifecta gate so it is
  quarantined as DATA and can never reach an egress/write sink without approval.
  Wired into `agent_session_host` behind the default-OFF `[security]` flag
  (byte-identical when disabled); the existing MCP/fetch taint tagging and the
  trifecta gate already cover mounted-untrusted-connector quarantine, now proven
  by tests. Adds the `security_stamp_directive` / `security_verify_directive`
  builtins and a `trigger_directive_provenance_gate` conformance fixture.
