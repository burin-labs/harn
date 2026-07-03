- **`harn models lora export` now preserves grouped tool results.** Structured
  LoRA exports convert multiple `[result of ...]` blocks in one user message
  into ordered tool-role messages instead of collapsing the group into prose.
