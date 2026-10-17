- `min` and `max` compare every argument: `min(5, 3, 1)` is `1`, not `3`.
- `format(template, dict)` keeps substituting after a literal `{`, so
  `format("{\"a\": {x}}", {x: 1})` is `{"a": 1}`, at runtime and in const-eval.
- `gzip_decode` reads every member of a concatenated gzip stream and, like `gzip -d`,
  ignores trailing bytes such as zero padding.
- `tar_create` accepts entry paths longer than 100 bytes by writing a GNU long-name record,
  and refuses absolute or `..` paths at every length.
- `csv_stringify` writes the ragged rows `csv_parse` returns, and writes `nil` cells as empty.
- `map_range(50, 0, 100, 0, 1)` is `0.5` instead of `0`, and `rad_to_deg` accepts floats.
- `ansi_strip` removes colon-form truecolor and `>`/`<`-prefixed CSI sequences, and keeps
  the visible text of ST-terminated OSC-8 hyperlinks.
- Path globs treat `?` as one character, so `?.rs` matches `é.rs`; name globs treat `\`
  as an escape on every platform and on every fast path.
- Monthly, quarterly, and yearly `date_range` ticks stay anchored to the start day after
  a short month (Jan 31, Feb 29, Mar 31), and `business_days_between` with reversed
  arguments returns the negated count.
- LLM mock miss errors and eval-pack output excerpts truncate within their budget with `…`.
- Trigger inbox dedupe TTLs and worker-queue leases read the mockable clock.
- Builtin reference tables and guides name the `harness.*` forms of channel, atomic,
  supervisor, daemon, trigger, trust, tracing, tool-hook, and lifecycle builtins.
