#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixtures_dir="${HARN_BENCH_FIXTURES_DIR:-$repo_root/perf/vm}"
iterations="${HARN_BENCH_ITERATIONS:-20}"
baseline_file=""
build_release=1
harn_bin="${HARN_BIN:-}"
mode="loop"
startup_runs="${HARN_BENCH_STARTUP_RUNS:-5}"
profile_json_dir="${HARN_BENCH_PROFILE_JSON_DIR:-}"
startup_helper="$repo_root/scripts/bench_vm_startup.harn"

usage() {
  cat <<'EOF'
Usage: scripts/bench_vm.sh [--iterations N] [--baseline FILE] [--no-build]
                           [--cold-start | --warm-start] [--startup-runs N]
                           [--profile-json-dir DIR]

Runs the deterministic VM microbenchmark fixture set with the release harn
binary and prints one row per benchmark.

Modes:
  (default)        Use `harn bench` to time the inner pipeline loop,
                   excluding parse/compile cost. Best for VM-only signal.
  --cold-start     Time end-to-end `harn run` with the bytecode cache
                   wiped between runs. Captures parse + typecheck +
                   compile + VM cost — the latency the user sees on a
                   fresh checkout or after a cache eviction.
  --warm-start     Time `harn run` with the cache pre-warmed. Captures
                   parse-skip + bytecode-load + VM cost — the latency a
                   developer feels on every subsequent invocation.

Options:
  -n, --iterations N    `harn bench` iterations per fixture (default: 20)
  --startup-runs N      cold/warm-start measurement runs per fixture
                        (default: 5)
  --profile-json-dir DIR
                        In loop mode, also write `harn bench --profile-json`
                        rollups to DIR/<fixture>.json
  --baseline FILE       Markdown baseline table to compare average wall time
  --no-build            Skip cargo build --release --bin harn
  -h, --help            Show this help

Environment:
  HARN_BIN                  Override the harn binary path
  HARN_BENCH_ITERATIONS     Default iteration count for loop mode
  HARN_BENCH_STARTUP_RUNS   Default per-fixture runs for cold/warm modes
  HARN_BENCH_FIXTURES_DIR   Override fixture directory
  HARN_BENCH_PROFILE_JSON_DIR
                            Default --profile-json-dir
  HARN_BENCH_HELPER_BIN     Harn binary used to run the startup timing helper
  HARN_BENCH_HELPER_CACHE_DIR
                            Cache directory for the startup timing helper
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
    --cold-start)
      mode="cold-start"
      shift
      ;;
    --warm-start)
      mode="warm-start"
      shift
      ;;
    --startup-runs)
      if [[ $# -lt 2 ]]; then
        echo "error: --startup-runs requires a value" >&2
        exit 2
      fi
      startup_runs="${2:-}"
      shift 2
      ;;
    --profile-json-dir)
      if [[ $# -lt 2 ]]; then
        echo "error: --profile-json-dir requires a value" >&2
        exit 2
      fi
      profile_json_dir="${2:-}"
      shift 2
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

if ! [[ "$startup_runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: --startup-runs must be a positive integer" >&2
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

if [[ -n "$profile_json_dir" ]]; then
  mkdir -p "$profile_json_dir"
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

measure_startup_runs() {
  local fixture="$1"
  local runs="$2"
  local helper_cache_dir="${HARN_BENCH_HELPER_CACHE_DIR:-$repo_root/target/harn-bench-helper-cache}"
  local helper_bin="${HARN_BENCH_HELPER_BIN:-$harn_bin}"
  mkdir -p "$helper_cache_dir"
  HARN_CACHE_DIR="$helper_cache_dir" "$helper_bin" run "$startup_helper" -- \
    --harn-bin "$harn_bin" \
    --fixture "$fixture" \
    --runs "$runs" \
    --cache-dir "$bench_cache_dir"
}

clear_cache_dir() {
  if [[ -n "${HARN_CACHE_DIR:-}" && -d "$HARN_CACHE_DIR" ]]; then
    rm -rf "$HARN_CACHE_DIR"
  fi
}

if [[ "$mode" == "loop" ]]; then
  printf "%-28s %10s %10s %10s %10s" "benchmark" "iterations" "min_ms" "avg_ms" "max_ms"
  if [[ -n "$baseline_file" ]]; then
    printf " %14s %10s" "baseline_avg" "delta"
  fi
  printf "\n"

  status=0
  for fixture in "${fixtures[@]}"; do
    benchmark="$(basename "$fixture" .harn)"
    bench_args=(bench "$fixture" --iterations "$iterations")
    if [[ -n "$profile_json_dir" ]]; then
      bench_args+=(--profile-json "$profile_json_dir/$benchmark.json")
    fi
    output="$("$harn_bin" "${bench_args[@]}")" || status=$?
    if [[ "$status" -ne 0 ]]; then
      printf "%s\n" "$output" >&2
      exit "$status"
    fi

    wall_line="$(awk '/^Wall time:/ { print; exit }' <<<"$output")"
    min_ms="$(extract_metric "$wall_line" "min")"
    avg_ms="$(extract_metric "$wall_line" "mean")"
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
  exit 0
fi

bench_cache_dir="${HARN_BENCH_CACHE_DIR:-$repo_root/target/harn-bench-cache}"
export HARN_CACHE_DIR="$bench_cache_dir"
mkdir -p "$bench_cache_dir"

printf "%-28s %10s %10s %10s %10s\n" \
  "benchmark[$mode]" "runs" "min_ms" "avg_ms" "max_ms"

for fixture in "${fixtures[@]}"; do
  benchmark="$(basename "$fixture" .harn)"
  case "$mode" in
    cold-start)
      # Wipe the cache between every measurement so each run pays the
      # full parse+compile cost.
      samples_min="" samples_avg="" samples_max=""
      mn="" avg="" mx=""
      total_min="" total_max="" total_sum="0"
      for ((i = 0; i < startup_runs; i++)); do
        clear_cache_dir
        mkdir -p "$bench_cache_dir"
        read -r elapsed_ms _avg _max < <(measure_startup_runs "$fixture" 1)
        if [[ -z "${total_min}" ]] || awk "BEGIN{exit !($elapsed_ms < $total_min)}"; then
          total_min="$elapsed_ms"
        fi
        if [[ -z "${total_max}" ]] || awk "BEGIN{exit !($elapsed_ms > $total_max)}"; then
          total_max="$elapsed_ms"
        fi
        total_sum="$(awk -v s="$total_sum" -v v="$elapsed_ms" 'BEGIN{printf "%.6f", s+v}')"
      done
      avg="$(awk -v s="$total_sum" -v n="$startup_runs" 'BEGIN{printf "%.3f", s/n}')"
      mn="$total_min"
      mx="$total_max"
      ;;
    warm-start)
      # Warm the cache once, then time `startup_runs` invocations.
      clear_cache_dir
      mkdir -p "$bench_cache_dir"
      "$harn_bin" run "$fixture" >/dev/null
      read -r mn avg mx < <(measure_startup_runs "$fixture" "$startup_runs")
      ;;
  esac

  if [[ ! "$mn" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    echo "error: failed to measure $fixture" >&2
    exit 1
  fi
  printf "%-28s %10s %10s %10s %10s\n" \
    "$benchmark" "$startup_runs" "$mn" "$avg" "$mx"
done
