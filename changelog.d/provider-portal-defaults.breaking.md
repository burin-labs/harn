- **The provider catalog artifact (schema v13) carries each provider's
  setup-UI default model.** `portal_defaults` maps a provider to the model a
  host offers when a person picks it, read from `provider_defaults.<p>.portal`.
  It is separate from `qc_defaults`, the cheap quality-check route. The
  Anthropic setup-UI default moves to `claude-sonnet-5-5` and the OpenAI one to
  `gpt-6.1-sol`. Runtime fallbacks are unchanged.

  Migration: regenerate vendored catalog artifacts and bindings at v13. Code
  that builds a `ProviderCatalogArtifact` literal adds
  `portal_defaults: BTreeMap::new()`.
