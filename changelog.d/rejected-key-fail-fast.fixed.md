- **A rejected API key is not sent again by the same process.** After a provider
  rejects a key (401, or a 403 whose body says the key is invalid or revoked),
  later calls to that provider with the same key fail locally with the same
  `auth` / `auth_failure` classification and `attempt_count: 0`, instead of
  paying another round trip for the same answer. Any other 403, such as a key
  without access to one model, blocks only that model, so a fallback to a model
  the key can use is still sent. Provider HTTP auth errors now carry
  `credential_scope: "key" | "request"`. A different key is always sent, and a
  refused key or model is tried again after five minutes.
