- **`harn models lora inspect` now applies provider overrides consistently.**
  The report recomputes the effective tool format for the selected provider and
  model route, so LoRA launch hints no longer mix an alias's original provider
  metadata with an explicit `--provider` override.
