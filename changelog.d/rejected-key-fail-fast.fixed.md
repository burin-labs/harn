- **A rejected API key is not sent again by the same process.** After a provider
  answers a key with 401/403, later calls to that provider with the same key fail
  locally with the same `auth` / `auth_failure` classification and
  `attempt_count: 0`, instead of paying another round trip for the same answer.
  A different key is still sent, and the rejected key is tried again after five
  minutes.
