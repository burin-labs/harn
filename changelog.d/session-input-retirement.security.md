- **Session secrets are revoked on eviction and reset.** Reopening a retired
  session cannot recover its private inputs, and delayed cleanup cannot revoke
  a replacement grant. AWS request signing preserves private header redaction
  in recorded requests (#10222).
