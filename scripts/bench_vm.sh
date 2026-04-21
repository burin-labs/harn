#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixtures_dir="${HARN_BENCH_FIXTURES_DIR:-$repo_root/perf/vm}"
iterations="${HARN_BENCH_ITERATIONS:-20}"
baseline_file=""
build_release=1
harn_bin="${HARN_BIN:-}"

usage() {
  cat <<'EOF'
Usage: scripts/bench_vm.sh [--iterations N] [--baseline FILE] [--no-build]

Runs the deterministic VM microbenchmark fixture set with the release harn
binary and prints one row per benchmark.

Options:
  -n, --iterations N  Number of harn bench iterations per fixture (default: 20)
  --baseline FILE     Markdown baseline table to compare average wall time
  --no-build          Skip cargo build --release --bin harn
  -h, --help          Show this help

Environment:
  HARN_BIN                  Override the harn binary path
  HARN_BENCH_ITERATIONS     Default iteration count
  HARN_BENCH_FIXTURES_DIR   Override fixture directory
  CARGO_TARGET_DIR          Cargo target directory for release builds
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -n|--iterations)
      if [[ $# -lt 2 ]]; then
        echo "error: --iterations requires a value" >&2
        exit 2
      fi
      iterations="${2:-}"
      shift 2
      ;;
    --baseline)
      if [[ $# -lt 2 ]]; then
        echo "error: --baseline requires a file path" >&2
        exit 2
      fi
      baseline_file="${2:-}"
      shift 2
      ;;
    --no-build)
      build_release=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if ! [[ "$iterations" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: --iterations must be a positive integer" >&2
  exit 2
fi

if [[ -n "$baseline_file" && ! -f "$baseline_file" ]]; then
  echo "error: baseline file not found: $baseline_file" >&2
  exit 2
fi

if [[ ! -d "$fixtures_dir" ]]; then
  echo "error: fixture directory not found: $fixtures_dir" >&2
  exit 2
fi

if [[ "$build_release" -eq 1 ]]; then
  cargo build --release --bin harn
fi

if [[ -z "$harn_bin" ]]; then
  target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
  harn_bin="$target_dir/release/harn"
fi

if [[ ! -x "$harn_bin" ]]; then
  echo "error: harn binary not found or not executable: $harn_bin" >&2
  exit 1
fi

shopt -s nullglob
fixtures=("$fixtures_dir"/*.harn)
shopt -u nullglob
if [[ "${#fixtures[@]}" -eq 0 ]]; then
  echo "error: no .harn fixtures found in $fixtures_dir" >&2
  exit 2
fi

baseline_avg_for() {
  local benchmark="$1"
  local file="$2"
  awk -F'|' -v name="$benchmark" '
    function trim(value) {
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
      return value
    }
    trim($2) == name {
      print trim($5)
      exit
    }
  ' "$file"
}

extract_metric() {
  local line="$1"
  local key="$2"
  sed -nE "s/.*${key} ([0-9]+([.][0-9]+)?) ms.*/\\1/p" <<<"$line"
}

printf "%-28s %10s %10s %10s %10s" "benchmark" "iterations" "min_ms" "avg_ms" "max_ms"
if [[ -n "$baseline_file" ]]; then
  printf " %14s %10s" "baseline_avg" "delta"
fi
printf "\n"

status=0
for fixture in "${fixtures[@]}"; do
  benchmark="$(basename "$fixture" .harn)"
  output="$("$harn_bin" bench "$fixture" --iterations "$iterations")" || status=$?
  if [[ "$status" -ne 0 ]]; then
    printf "%s\n" "$output" >&2
    exit "$status"
  fi

  wall_line="$(awk '/^Wall time:/ { print; exit }' <<<"$output")"
  min_ms="$(extract_metric "$wall_line" "min")"
  avg_ms="$(extract_metric "$wall_line" "avg")"
  max_ms="$(extract_metric "$wall_line" "max")"
  if [[ ! "$min_ms" =~ ^[0-9]+([.][0-9]+)?$ || ! "$avg_ms" =~ ^[0-9]+([.][0-9]+)?$ || ! "$max_ms" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    echo "error: failed to parse wall-time metrics from harn bench output for $fixture" >&2
    printf "%s\n" "$output" >&2
    exit 1
  fi

  printf "%-28s %10s %10s %10s %10s" "$benchmark" "$iterations" "$min_ms" "$avg_ms" "$max_ms"
  if [[ -n "$baseline_file" ]]; then
    baseline_avg="$(baseline_avg_for "$benchmark" "$baseline_file")"
    if [[ "$baseline_avg" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
      delta="$(awk -v current="$avg_ms" -v baseline="$baseline_avg" 'BEGIN { printf "%+.1f%%", ((current - baseline) / baseline) * 100.0 }')"
      printf " %14s %10s" "$baseline_avg" "$delta"
    else
      printf " %14s %10s" "-" "-"
    fi
  fi
  printf "\n"
done
