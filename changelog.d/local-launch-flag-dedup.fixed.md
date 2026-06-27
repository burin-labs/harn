- **`harn local launch` no longer duplicates flags that a runtime's
  `default_args` and a dedicated CLI flag both supply.** The llama.cpp runtime
  ships `default_args = [--jinja, --reasoning off, --reasoning-format deepseek,
  --metrics, --flash-attn on]`; passing the matching dedicated flags
  (`--jinja`, `--flash-attn on`, ...) appended each one a second time, so the
  launched argv carried `--jinja ... --jinja` and `--flash-attn on ...
  --flash-attn on`. The builder now folds in only the `default_args` entries the
  caller did not override, and the explicit value wins (e.g. `--flash-attn auto`
  replaces the default `on`). Harmless to llama.cpp, but it made the persisted
  launch record and logs misleading; deduped output is now exact.
