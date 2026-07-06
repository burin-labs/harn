- **Guard all outbound HTTP clients against SSRF.** The rebind-proof
  connect-time SSRF resolver is now installed on every outbound client — the
  shared LLM streaming/blocking/utility clients, connector clients, MCP
  discovery/OAuth/card/HTTP-transport clients, the provider healthcheck, and the
  remote provider-catalog fetcher — not just the `http_*` builtins. A base URL,
  connector endpoint, or MCP server URL whose hostname resolves (or DNS-rebinds)
  to a private / loopback / link-local / metadata (`169.254.169.254`) /
  denied-CIDR address is now unreachable at connect time on these paths.
- **Redaction no longer passes secrets in values larger than 256 KB.** Oversized
  string values were previously returned unredacted; a secret embedded in a
  large tool result, transcript, or base64 blob under a non-sensitive field name
  could leak verbatim. Oversized inputs are now scanned in overlapping windows,
  so a secret anywhere in the value — including one straddling a window boundary
  — is redacted, while legitimate large content is not over-redacted.
- **Corrected the `std/session-store` integrity claims.** The keyless SHA-256
  hash chain is documented as tamper-EVIDENT (detects accidental corruption,
  truncation, reordering, and naive edits) rather than tamper-RESISTANT: a
  writer with filesystem access can rewrite history and recompute the chain, and
  `actor` / `tenant_id` / `ts_ms` are not part of the digest. Use signed
  harn-serve session receipts / Ed25519 run-receipt provenance for attribution.
  The record-hash formula is unchanged (cross-language contract with Burin).
