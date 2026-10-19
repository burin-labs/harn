# Captured compiler diagnostics

`std/diagnostics.diagnostics_decode(tools, request)` decodes captured output.
It starts no process and reads no file. Hosts keep command execution, permissions,
capture limits, and presentation in their existing adapters.

The request supplies `format`, `stdout`, `stderr`, nullable `exit_code`,
`completed`, and `capture_complete`. Optional `source_text` maps Harn byte spans
to source lines. Formats are `cargo-json`, `go`, `generic`, `positional`, `ruff`,
`shellcheck`, `scala3`, `php-lint`, `zsh`, and `harn-json`.

The result includes located `diagnostics`, `measured`, `reason`, and nullable
`error_count`. Only a completed process with an exit status and complete capture
can measure errors. Unknown formats, malformed JSON, and failed commands without
recognized errors remain unmeasured. Partial findings remain visible, with a null
error count. A documented empty JSON array from a completed zero exit measures
zero errors. Text formats use the compiler exit status to establish a clean check.

`tools.run_build_command` uses this same decoder and reports whether its
diagnostics were measured in `diagnostics_complete`.
