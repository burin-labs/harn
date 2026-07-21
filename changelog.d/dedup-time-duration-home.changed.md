- **One owner for RFC3339 timestamps and for `~`/`$HOME` expansion.**
  - `harn-clock` now owns RFC3339 rendering (`format_rfc3339`, `now_rfc3339`,
    `system_now_rfc3339`). Roughly twenty hand-rolled `now_rfc3339` copies
    across the VM, CLI, hostlib, and serve adapters route through it, so a
    `PausedClock` pins the rendered timestamp wherever a `Clock` is in scope.
    The renderer is infallible: `time`'s formatter cannot fail for a wall-clock
    UTC instant, and the copies that papered over that branch with an empty
    string, a `Display` rendering, or a random UUID now all emit
    `1970-01-01T00:00:00Z`.
  - `harn_vm::user_dirs` now owns home-directory expansion (`expand_home`,
    `expand_home_from`, `expand_home_path`) and handles the full `~`, `~/`,
    `$HOME`, `$HOME/` set. `harn local launch` and LoRA path normalization read
    `$HOME` directly and so resolved nothing on Windows; both are fixed.
    `~alice` and `$HOMEBREW` are held back from expanding.
