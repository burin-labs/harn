#!/usr/bin/env bash
set -euo pipefail

# Strict Clippy runs as three legs because each resolves a different feature
# graph and none can reuse another's check units. CI gives every leg its own
# runner so the lane costs its slowest leg instead of their sum; the release
# gate and local callers run all three in order.
#
# Usage: scripts/ci/run_rust_lint_lane.sh [--leg all|workspace|lean-lsp|freshness-checker]
#
# The CI matrix in ci.yml:rust-checks names these legs; check-ci-cache-policy
# holds the matrix to exactly this set.
leg="all"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --leg)
      [[ $# -ge 2 ]] || { echo "error: --leg requires a value" >&2; exit 2; }
      leg="$2"
      shift 2
      ;;
    *)
      echo "usage: $0 [--leg all|workspace|lean-lsp|freshness-checker]" >&2
      exit 2
      ;;
  esac
done

lint_workspace() {
  # Cargo does not inspect source contents once its timestamp-based fingerprint
  # says a unit is fresh, and it does not replay diagnostics from that prior
  # compile. A restored target/build directory can therefore contain a
  # warning-clean workspace unit whose artifact is newer than changed checkout
  # source; the strict invocation below then exits successfully without running
  # Clippy on that unit. Keep dependency artifacts warm, but invalidate every
  # workspace package at the lint boundary so the proof always reaches Clippy.
  cargo clean --workspace
  cargo clippy --workspace --all-targets -- -D warnings
}

lint_lean_lsp() {
  # The workspace sweep proves nothing about the lean feature slice. Cargo
  # unifies features across the packages it builds together, so `--workspace`
  # resolves one `harn-vm` carrying the union of every member's request — that
  # is `full`, because the CLI asks for it. `harn-lsp` declares
  # `harn-vm = { default-features = false }` and ships against a much smaller
  # graph, and code reachable only from a builtin family behind an optional
  # feature compiles there with no caller at all.
  #
  # Nothing else caught that on an ordinary Rust change: the editor job that
  # does build `harn-lsp` lean is path-gated to the CI workflow and
  # `editors/vscode/**`, and the lean-embedding workflow reads dependency-graph
  # shape with `cargo tree` without compiling. A `src/`-only change could
  # therefore land dead code in a shipped configuration and leave main broken
  # until an unrelated PR happened to touch a gated path and inherit the red
  # (#7017). This lane already runs on every Rust source change, so resolving
  # the package on its own here puts the check where its trigger scope covers
  # what it reads.
  cargo clippy -p harn-lsp -- -D warnings
}

lint_freshness_checker() {
  # The freshness checker is a feature-gated binary, so the workspace sweep
  # never compiles it. `make lint` covers it and this lane must too: its 1 MiB
  # stack buffer tripped clippy's large_stack_arrays lint locally and
  # overflowed the Windows main thread in CI, where no lint had run on it
  # (harn#8893).
  cargo clippy -p harn-cli --bin harn-freshness-check \
    --features internal-freshness-checker -- -D warnings
}

case "$leg" in
  all)
    lint_workspace
    lint_lean_lsp
    lint_freshness_checker
    ;;
  workspace) lint_workspace ;;
  lean-lsp) lint_lean_lsp ;;
  freshness-checker) lint_freshness_checker ;;
  *)
    echo "error: unknown --leg '$leg' (expected all, workspace, lean-lsp, or freshness-checker)" >&2
    exit 2
    ;;
esac
