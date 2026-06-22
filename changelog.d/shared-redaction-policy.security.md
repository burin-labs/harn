- **Redaction-sensitive orchestration surfaces now share Harn's provider-aware
  secret policy.** Crystallization bundles and friction/context-pack records
  reuse the central redaction catalog for provider tokens, JWTs, private keys,
  and sensitive key/value assignments while still preserving logical secret
  references such as `github/webhook-secret`.
