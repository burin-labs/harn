#!/usr/bin/env bash
#
# Capture clippy's raw JSON diagnostics under a lowered stack-frame threshold,
# for `scripts/check_stack_frames.harn` to reduce and judge.
#
# The workspace lint keeps its own ceiling. This runs clippy under a private
# config directory holding a lower `stack-size-threshold`, so the gate sees a
# frame growing long before it reaches the ceiling that aborts a worker.
#
# Usage: collect_stack_frames.sh <raw-json-out> [threshold-bytes]
set -euo pipefail

raw_out="${1:?usage: collect_stack_frames.sh <raw-json-out> [threshold-bytes]}"
threshold="${2:-100000}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

conf_dir="$(mktemp -d)"
trap 'rm -rf "$conf_dir"' EXIT
if [ -f clippy.toml ]; then
  cp clippy.toml "$conf_dir/clippy.toml"
fi
printf 'stack-size-threshold = %s\n' "$threshold" >> "$conf_dir/clippy.toml"

# The census is a measurement, not the lint gate. `-D warnings` is dropped and
# clippy's exit status ignored on purpose: at a lowered threshold this lint
# fires by design, and under the workspace RUSTFLAGS that would abort the run
# before the diagnostics were written. The lint gate itself lives in the Rust
# lint lane and keeps its own ceiling. The only pass condition here is the
# non-null control below.
set +e
CLIPPY_CONF_DIR="$conf_dir" RUSTFLAGS="" cargo clippy --workspace --all-targets \
  --message-format=json > "$raw_out"
cargo_status=$?
set -e

if [ ! -s "$raw_out" ]; then
  echo "error: clippy wrote no output (exit ${cargo_status}); the census measured nothing." >&2
  exit 1
fi

if ! grep -q 'clippy::large_stack_frames' "$raw_out"; then
  echo "error: clippy reported no large_stack_frames diagnostics at ${threshold} bytes." >&2
  echo "A silent census cannot distinguish a clean tree from a run that measured nothing." >&2
  echo "hint: clippy replays cached diagnostics, so a target directory warmed at a" >&2
  echo "different threshold reports the previous run. Re-run against a cold target." >&2
  exit 1
fi
echo "stack-frame census captured at a ${threshold}-byte threshold"
