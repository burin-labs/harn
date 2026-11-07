- **The provider catalog (schema v16) ranks a mid-tier automatic default per credentialed provider.**
  The artifact gains `automatic_defaults`, an ordered list of provider and model pairs.
  A host that was given no model takes the first entry whose provider it holds a credential for
  (Claude Sonnet 5.5 for Anthropic, then GPT-6.1 Sol for OpenAI) and falls back to a local model
  when none matches. Authored as `automatic` and `automatic_order` under `provider_defaults.<provider>`.

  Migration: a host that decodes the artifact regenerates its vendored copy at schema v16, and Rust
  code that builds `ProviderCatalogArtifact` or `ProviderDefaultModels` with a struct literal adds
  the new fields (`automatic_defaults`; `automatic`, `automatic_order`).

  ```text
  schema_version: 15   # before
  schema_version: 16   # after
  ```
