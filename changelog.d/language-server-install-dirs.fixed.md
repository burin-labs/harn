`rename_symbol` now finds a language server that `PATH` does not name. After
the override variable, the project's bin directory, and `PATH`, it searches
`$CARGO_HOME/bin` and the selected rustup toolchain, Go's `GOBIN` and
`GOPATH/bin`, npm's global prefixes, pnpm, Volta, Bun, `~/.local/bin`, and the
Homebrew directories, so an app opened from Finder or a non-login shell reaches
`rust-analyzer` in `~/.cargo/bin`. A rustup proxy whose toolchain lacks the
component, and a `tsc` older than TypeScript 7, are skipped with a reason
instead of failing at startup. The server starts with its own directory first
on `PATH` and with `CARGO_HOME` and `RUSTUP_HOME` set. An override naming a
missing file still forces the syntax-tree fallback.
