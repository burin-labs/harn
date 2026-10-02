# Generated CLI AOT payload

`make gen-cli-aot` writes the release and package CLI bytecode payload here:
`cli-bytecode-manifest.json` and `cli-bytecode/`. Both are git-ignored and are
embedded by `build.rs` when present.

This file is tracked so the directory exists in every checkout. `build.rs`
watches the directory, and if a build had to create it, Cargo would see a
watched path newer than the build script's start and recompile `harn-cli` on
the next Cargo command.
