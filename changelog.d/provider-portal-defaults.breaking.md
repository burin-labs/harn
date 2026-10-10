- **The provider catalog artifact (schema v13) carries each provider's
  setup-UI default model.** `portal_defaults` maps a provider to the model a
  host offers when a person picks it, read from `provider_defaults.<p>.portal`.
  Each entry names `model_id` and, where the setup UI selects one,
  `reasoning_effort` (from `provider_defaults.<p>.portal_reasoning_effort`).
  It is separate from `qc_defaults`, the cheap quality-check route. The setup-UI
  defaults move to the current generation: Anthropic `claude-sonnet-5-5`,
  OpenAI `gpt-6-luna` at `high` effort, and Ollama `gemma4:26b`, the `local`
  variant's row. Runtime fallbacks are unchanged.

  Migration: regenerate vendored catalog artifacts and bindings at v13. Code
  that builds a `ProviderCatalogArtifact` literal adds
  `portal_defaults: BTreeMap::new()`, and code that builds a
  `ProviderDefaultModels` literal adds `portal_reasoning_effort: None`.
