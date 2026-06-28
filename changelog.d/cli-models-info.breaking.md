- **`harn model-info` is folded into `harn models info`.** The standalone
  top-level `model-info` command is removed; the same model-metadata lookup now
  lives under the `models` noun (`harn models info <model> [--verify] [--warm]`),
  matching the `provider`/`skill` consolidations. No alias is kept.
