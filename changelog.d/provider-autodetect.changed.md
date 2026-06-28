- When no provider is configured (`HARN_DEFAULT_PROVIDER` and `default_provider` both unset), Harn now
  auto-selects a provider instead of silently assuming Anthropic: it prefers a configured cloud provider whose
  API key is present, then a local/auth-free provider (Ollama, `harn local`), and only then falls back to the
  documented Anthropic default — warning once (with how to configure one) so non-Anthropic adopters get a clear
  nudge rather than a raw auth failure. Detection reads catalog `auth_env`/`local_runtime` metadata only, with
  no hardcoded paths or ports.
