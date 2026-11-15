- Forge pull-request normalization retains decoded envelope, repository and
  revision contracts while preserving raw fields, scalar normalization and
  provider-specific fallbacks. Connector binding matches keep their existing
  duplicate-path refusal.
- CI workflow helpers handle absent strategy and action-run containers before
  decoding hyphenated keys, preserving false, zero and empty-string values.
