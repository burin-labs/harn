- **Tool-format probes now reserve visible-output headroom after model reasoning.**
  `harn provider tool-probe` and `tool-calibrate` no longer starve reasoning
  routes with a flat 256-token fallback and misclassify capable large-string
  tool calls as `empty_silent`.
