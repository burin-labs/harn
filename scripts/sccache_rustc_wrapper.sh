#!/bin/sh
# rustc wrapper that lets one sccache cache serve every Harn worktree.
#
# sccache 0.17 folds every environment variable named CARGO_* into its Rust
# cache key. `dev_setup.sh` gives each worktree its own absolute
# CARGO_TARGET_DIR, and `cargo_with_worktree_build_dir.sh` exports that value
# for every Cargo entry point, so two worktrees compiling the byte-identical
# registry dependency produced two different keys and neither could ever read
# the other's entry. Configuring `rustc-wrapper = "sccache"` directly therefore
# measured a 0% Rust hit rate no matter how large the shared cache was.
#
# rustc itself never reads CARGO_TARGET_DIR; Cargo alone uses it to decide
# where output goes, and Cargo still sees it because only this wrapper's own
# environment is modified. Dropping it here is what makes the key depend on the
# compilation rather than on which checkout requested it.
#
# Measured on an isolated cache with four registry dependencies, building the
# same sources into two different target directories:
#
#   rustc-wrapper = "sccache"     0 hits / 8 misses
#   rustc-wrapper = this script   4 hits / 4 misses
#
# Workspace-local crates still miss across worktrees: sccache also hashes the
# compilation's working directory, which is the checkout path by construction.
# That is not configurable in sccache 0.17, so this recovers the dependency
# graph, which is the large majority of a cold build's compilations.
unset CARGO_TARGET_DIR
exec sccache "$@"
