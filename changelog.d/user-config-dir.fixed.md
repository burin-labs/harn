- A call that names only `provider: "anthropic"` now resolves Anthropic's own
  default model, even when a user overlay makes another provider the default.
  Previously that overlay left Anthropic with no default, and a provider with
  no authored default could be handed Anthropic's model. Every provider's
  default is now its `provider_defaults.<provider>.runtime` entry. The retired
  top-level `fallback_model` key is still accepted in overlays and becomes the
  default provider's `runtime` entry.
- `providers.toml`, `mcp_presets.toml`, `mcp_bulk_auth.toml`, and user skills
  now honor `$XDG_CONFIG_HOME`, as `config.toml` already did, and `harn config`
  and the runtime now agree on where `providers.toml` lives. Setting
  `XDG_CONFIG_HOME` to an empty directory runs Harn with no user configuration.
