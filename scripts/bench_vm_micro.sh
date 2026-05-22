#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bench_name="bench_vm_hot_paths"
target_dir="${CARGO_TARGET_DIR:-}"

usage() {
  cat <<'EOF'
Usage: scripts/bench_vm_micro.sh [--bench NAME] [--target-dir DIR] [CRITERION_FILTER ...]

Runs the allocation-aware Criterion VM microbenchmarks for targeted
interpreter hot-path work. Arguments after the options are passed through to
Criterion as filters, for example:

  scripts/bench_vm_micro.sh property_inline_cache_hits
  scripts/bench_vm_micro.sh -- method_inline_cache_hits

Options:
  --bench NAME        Criterion bench target (default: bench_vm_hot_paths)
  --target-dir DIR    Cargo target directory. Defaults to a fresh temp dir.
  -h, --help          Show this help

Environment:
  CARGO_TARGET_DIR    Reuse an explicit target dir instead of a temp dir

Criterion timing artifacts are written under $CARGO_TARGET_DIR/criterion.
Allocation records are emitted as JSON Lines on stderr before timing starts.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bench)
      if [[ $# -lt 2 ]]; then
        echo "error: --bench requires a value" >&2
        exit 2
      fi
      bench_name="$2"
      shift 2
      ;;
    --target-dir)
      if [[ $# -lt 2 ]]; then
        echo "error: --target-dir requires a value" >&2
        exit 2
      fi
      target_dir="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    *)
      break
      ;;
  esac
done

if [[ -z "$target_dir" ]]; then
  target_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-vm-microbench-target.XXXXXX")"
fi

export CARGO_TARGET_DIR="$target_dir"
export CARGO_PROFILE_BENCH_LTO="${CARGO_PROFILE_BENCH_LTO:-false}"
export CARGO_PROFILE_BENCH_CODEGEN_UNITS="${CARGO_PROFILE_BENCH_CODEGEN_UNITS:-16}"

cd "$repo_root"
cargo bench -p harn-vm-perf --bench "$bench_name" -- "$@"
