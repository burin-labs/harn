- LLM provider dispatch: `resolve_api_key` and the `harn providers` status
  builtin no longer special-case Bedrock/Vertex by hardcoded provider name to
  skip the generic `auth_env` check. Providers now declare
  `credential_resolution = "platform_managed"` in `providers.toml` when their
  shim resolves credentials through a multi-step chain (AWS SigV4 credential
  chain, GCP ADC / service-account JSON) instead of a simple env-var lookup.
  This also fixes a latent gap where Vertex's declared `auth_env` list (which
  does not include every valid ADC path) could make `resolve_api_key` report
  a false "missing API key" outside the code paths that had the hardcoded
  bypass.
