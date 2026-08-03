CI lanes that ran Rust tests by invoking cargo directly no longer inherit
Rust's 2 MiB spawned-thread stack instead of the 16 MiB the rest of the suite
uses. `thread-parity.yml` and the flake-detection rerun both bypassed
`scripts/ci/run_rust_test_lane.sh`, so `harn-serve`'s A2A handoff test aborted
with `fatal runtime error: stack overflow` at every thread count — reporting a
stack-size difference as a thread-count parity failure, and keeping both
scheduled lanes red since 2026-07-27. Two more lanes (the Landlock escape
proof and the macOS package-hash parity check) had the same gap without a
visible failure. `make check-rust-test-lane-policy` now fails any workflow
step that runs Rust tests without either the lane wrapper or an explicit
`RUST_MIN_STACK`.
