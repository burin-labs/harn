- **Corrected the DeepInfra GLM-5.2 catalog pricing to its published rate.**
  The `deepinfra/zai-org/GLM-5.2` row carried `1.40/4.40/0.26` — the
  together/baseten placeholder copied onto the sibling DeepInfra row, not
  DeepInfra's own rate. DeepInfra's published GLM-5.2 pricing, verified against
  their pricing API and reconciled exactly against a live `chat/completions`
  `estimated_cost` (and corroborated by OpenRouter's `z-ai/glm-5.2` listing),
  is `$0.95` in / `$3.00` out / `$0.18` cached read per MTok. Catalog data only,
  no behavior change; this fixes cost accounting for the DeepInfra GLM-5.2 route.
