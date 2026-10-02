- A missing secret now names the provider chain that was consulted and any
  default provider the chain leaves out, for example
  `not found in providers: env (...); keyring disabled by HARN_SECRET_PROVIDERS=env`.
  Before this change, a keyring credential hidden by `HARN_SECRET_PROVIDERS=env`
  read as never stored.
- `harness.secrets.read` reports a secret that no provider in the chain holds
  as `not_found` instead of `tool_error`. An empty provider chain now reports
  `tool_error` instead of `not_found`, so `not_found` always means the secret
  is absent. `std/oauth/storage.secrets` now returns `nil` for an unstored
  token instead of throwing. The `std/oauth` client's "no token in storage"
  diagnostic includes the chain detail.
- `harn doctor` warns about each default secret provider that
  `HARN_SECRET_PROVIDERS` leaves out, and no longer reports the `file` provider
  as unsupported.
