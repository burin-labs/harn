Pin Fireworks-hosted `gpt-oss-*` to the `text` (heredoc) tool channel instead
of `json`. An empirical A/B (real Fireworks calls, 3 samples per arm, task =
author a backslash-heavy Zig file) showed the `json` channel corrupts source
bodies in every sample — gpt-oss double-escapes the backslashes a JSON string
arg requires (`\\` becomes `\\\\` Zig multiline prefixes, escaped quotes, and
one run leaked literal `\n`/`\"` for the whole file) — while the escape-free
heredoc body stayed byte-clean in every sample. Tool-call dispatch succeeded on
both channels (no heredoc-wrapper regression).
