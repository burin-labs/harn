- **`hostlib_code_index_*` `line_count` no longer overcounts files that end in a newline.**
  The code-index file scanner counted lines with `content.split('\n').count()`,
  which reports one phantom extra line for any file with a trailing newline (the
  common case) — e.g. a two-line file ending in `\n` was reported as 3 lines.
  Line counting is now shared through a single `count_lines` helper, matching the
  scanner and process-artifact surfaces that already counted correctly, so the
  `line_count` field surfaced to scripts is accurate.
