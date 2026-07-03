- **Untrusted-origin file taint (opt-in).** Under `taint_file_provenance`, a
  file written while untrusted content is in the session's context — or by a
  fetch / clone / MCP step — is recorded in a session-scoped provenance ledger,
  and a later read of that path is classified untrusted so it flows into the
  same lethal-trifecta gate as a live external ingress. This quarantines a
  deferred on-disk injection (a cloned dependency's `README`, a downloaded
  dataset) that a plain first-party file read would otherwise carry straight to
  an exfil sink. First-party file reads stay trusted (a file you authored is not
  an injection vector). The containment battery shows this lifts overall
  exfil-sink containment by exactly the on-disk file-read attack count; the
  default posture is byte-identical.
