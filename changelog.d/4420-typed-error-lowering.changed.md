Thrown errors now carry typed structure instead of only rendered prose. A
caught `CategorizedError` (e.g. a `tool_rejected` sandbox violation) lowers to a
`{category, message}` dict keyed by the canonical `ErrorCategory` string, and
the `fs` builtins that wrap an `io::Error` (`read_file`, `write_file`,
`append_file`, `mkdir`, `delete_file`, `copy_file`, `move_file`, `stat`,
`list_dir`, `read_lines`, `mkdtemp`, …) now throw
`{error: "io_error", kind, message}` with a stable `kind` (`storage_full`,
`quota_exceeded`, `not_found`, `permission_denied`, …). Consumers branch on the
typed fields instead of substring-matching English prose; a `catch` that
stringifies the value still renders sensibly because `message` is preserved.
