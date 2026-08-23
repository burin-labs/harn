#!/usr/bin/env bash
set -euo pipefail

# Cargo does not inspect source contents once its timestamp-based fingerprint
# says a unit is fresh, and it does not replay diagnostics from that prior
# compile. A restored target/build directory can therefore contain a
# warning-clean workspace unit whose artifact is newer than changed checkout
# source; the strict invocation below then exits successfully without running
# Clippy on that unit. Keep dependency artifacts warm, but invalidate every
# workspace package at the lint boundary so the proof always reaches Clippy.
cargo clean --workspace
cargo clippy --workspace --all-targets -- -D warnings

# The sweep above proves nothing about the lean feature slice. Cargo unifies
# features across the packages it builds together, so `--workspace` resolves
# one `harn-vm` carrying the union of every member's request — that is `full`,
# because the CLI asks for it. `harn-lsp` declares
# `harn-vm = { default-features = false }` and ships against a much smaller
# graph, and code reachable only from a builtin family behind an optional
# feature compiles there with no caller at all.
#
# Nothing else caught that on an ordinary Rust change: the editor job that does
# build `harn-lsp` lean is path-gated to the CI workflow and `editors/vscode/**`,
# and the lean-embedding workflow reads dependency-graph shape with `cargo tree`
# without compiling. A `src/`-only change could therefore land dead code in a
# shipped configuration and leave main broken until an unrelated PR happened to
# touch a gated path and inherit the red (#7017). This lane already runs on
# every Rust source change, so resolving the package on its own here puts the
# check where its trigger scope covers what it reads.
cargo clippy -p harn-lsp -- -D warnings
