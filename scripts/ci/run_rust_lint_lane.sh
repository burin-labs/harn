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
if [[ ${1:-} == --list-legs && $# == 1 ]]; then
  printf '%s\n' workspace lean-lsp freshness-checker
  exit 0
fi
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

case "$leg" in
  all|workspace|lean-lsp|freshness-checker) ;;
  *)
    echo "error: unknown --leg '$leg' (expected all, workspace, lean-lsp, or freshness-checker)" >&2
    exit 2
    ;;
esac

# Cargo does not inspect source contents once its timestamp-based fingerprint
# says a unit is fresh, and it does not replay diagnostics from that prior
# compile. A restored target/build directory can therefore contain a
# warning-clean workspace unit whose artifact is newer than changed checkout
# source; the strict invocation below then exits successfully without running
# Clippy on that unit. Keep dependency artifacts warm, but invalidate every
# workspace package at the lint boundary so every independent leg reaches
# Clippy. Local all mode invalidates once before its three graphs.
cargo clean --workspace

lint_workspace() {
  if [[ -z "${HARN_LINT_TIMINGS_DIR:-}" ]]; then
    cargo clippy --workspace --all-targets -- -D warnings
    return
  fi
  # Collect Cargo's unit timings from this invocation, not a restored report.
  # The ordinary strict compile is the only compile; no diagnostic rebuild runs.
  local source tree target report status bytes report_blob rustc sample_pid sample_dir sample_receipt started_at ended_at
  source="$(git rev-parse HEAD)"
  tree="$(git rev-parse 'HEAD^{tree}')"
  if [[ "$source" != "${HARN_LINT_SOURCE_SHA:-}" ]] || ! git diff --quiet HEAD --; then
    echo "error: lint timing source is not the requested clean commit" >&2
    return 1
  fi
  if [[ -e "$HARN_LINT_TIMINGS_DIR" || -L "$HARN_LINT_TIMINGS_DIR" ]]; then
    echo "error: lint timing output already exists" >&2
    return 1
  fi
  target="$(cargo metadata --no-deps --format-version 1 | jq -er '.target_directory | select(type == "string" and startswith("/"))')"
  report="$target/cargo-timings/cargo-timing.html"
  rm -f "$report"
  mkdir -p "$HARN_LINT_TIMINGS_DIR"
  sample_dir="$(mktemp -d "$HARN_LINT_TIMINGS_DIR/compiler-observation.XXXXXX")"
  sample_receipt="${sample_dir#"$HARN_LINT_TIMINGS_DIR/"}/compiler-sample.json"
  sample_pid=""
  if [[ "$(uname -s)" == Darwin ]]; then
    node "$(dirname "${BASH_SOURCE[0]}")/macos_compiler_sample.cjs" \
      "$$" "$sample_dir" "${GITHUB_RUN_ID:-}" "${GITHUB_RUN_ATTEMPT:-}" "$source" "$tree" &
    sample_pid=$!
  fi
  status=0
  started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  cargo clippy --workspace --all-targets --timings -- -D warnings || status=$?
  ended_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ -n "$sample_pid" ]]; then
    kill -TERM "$sample_pid" 2>/dev/null || true
    wait "$sample_pid" || true
  fi
  if [[ ! -f "$sample_dir/compiler-sample.json" ]]; then
    jq -n --arg source "$source" --arg tree "$tree" \
      '{schema:"harn.macos_compiler_samples.v1",sourceCommit:$source,sourceTree:$tree,status:"UNMEASURED",reason:"sampler unavailable or interrupted",samples:[]}' \
      > "$sample_dir/compiler-sample.json"
  fi
  if [[ ! -f "$report" || -L "$report" || ! -s "$report" ]]; then
    echo "error: strict compile did not produce a new nonempty timing report" >&2
    return 1
  fi
  bytes="$(wc -c < "$report" | tr -d ' ')"
  if [[ "$bytes" -gt 10485760 ]]; then
    echo "error: lint timing report exceeds 10 MiB" >&2
    return 1
  fi
  if [[ "$(git rev-parse HEAD)" != "$source" || "$(git rev-parse 'HEAD^{tree}')" != "$tree" ]] \
    || ! git diff --quiet HEAD --; then
    echo "error: source changed during the timed strict compile" >&2
    return 1
  fi
  report_blob="$(git hash-object "$report")"
  rustc="$(rustc -Vv)"
  cp "$report" "$HARN_LINT_TIMINGS_DIR/cargo-timing.html"
  jq -n --arg source "$source" --arg tree "$tree" \
    --arg report_blob "$report_blob" \
    --arg started_at "$started_at" --arg ended_at "$ended_at" \
    --arg sample_receipt "$sample_receipt" \
    --arg rustc "$rustc" --argjson status "$status" --argjson bytes "$bytes" \
    '{schema:"harn.strict_lint_timings.v1",sourceCommit:$source,sourceTree:$tree,
      leg:"workspace",command:["cargo","clippy","--workspace","--all-targets","--timings","--","-D","warnings"],
      compileStartedAt:$started_at,compileFinishedAt:$ended_at,
      compilerSampleReceipt:$sample_receipt,
      exitCode:$status,report:"cargo-timing.html",reportBytes:$bytes,reportGitBlob:$report_blob,rustc:$rustc}' \
    > "$HARN_LINT_TIMINGS_DIR/receipt.json"
  return "$status"
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
esac
