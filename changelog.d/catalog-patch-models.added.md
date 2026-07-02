- **`[patch.models]` field-wise catalog overlay patches.** Provider config
  overlays can now tweak individual model-row fields
  (`[patch.models."<id>"] stream_timeout = 1200.0`) instead of copying the
  whole baseline row verbatim and freezing its other fields against catalog
  updates. Tables merge recursively, scalars and arrays replace, patches win
  over same-overlay whole-row replacement, and they stay sticky across later
  layers' whole-row refreshes. Works at every overlay layer, including
  `harn.toml` `[llm.patch.models]`; dangling patches are held until the row
  arrives and reported via `dangling_model_patches()`.
