- `floor` and `ceil` accept decimals instead of raising a type error, and
  `min`/`max` promote an int operand beside a decimal instead of returning
  `nil`.
- `date_parse` honors the offset on a minute-precision ISO 8601 time such as
  `2026-04-25T17:32+02:00` instead of reading the offset hour as seconds.
- `path_relative_to` treats a `.` base as the current directory
  (`path_relative_to("src/a", ".")` is `src/a`, not `../src/a`) and returns
  `nil` for a base that climbs above it.
- `.pad_left` and `.pad_right` return the string unchanged for a negative
  width, matching `str_pad`, instead of raising an allocation error.
- Sanitizing a tool schema whose dropped `default`, `enum`, or `pattern`
  holds long non-ASCII text no longer panics.
- `harn check` no longer replays cached diagnostics after a rebuild that
  changed a builtin signature. The result-cache key now includes a digest of
  the builtin manifest, which neither source fingerprint covered.
