- **Process sandbox receipts can name a spawn broker.** `ProcessSandboxReport`
  and `ProcessSandboxReportingContext` gained `broker: Option<String>`, set when
  an embedder spawn broker ran the child outside the active confinement.

  Migration: Rust code constructing either struct with a literal adds
  `broker: None`. Serialized receipts are unchanged unless a broker ran the
  child, in which case they carry an additional `broker` string.
