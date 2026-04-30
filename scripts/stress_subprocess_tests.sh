#!/usr/bin/env bash
#
# Stress-test the harn-cli subprocess integration suite under the
# relaxed `harn-subprocess` / `harn-cli-bin` nextest group caps
# introduced by harn#949.
#
# Background: those test groups used to be capped at `max-threads = 4`
# to avoid macOS dyld + AMFI cold-cache scheduler starvation. The
# architectural fix is the per-process pre-warm in
# `crates/harn-cli/tests/test_util/process.rs::harn_command()`. This
# script gives reviewers a reproducible way to verify the fix holds:
# loop the harn-cli nextest run N times and report any flakes.
#
# Usage:
#   scripts/stress_subprocess_tests.sh                # 5 iterations, default profile
#   scripts/stress_subprocess_tests.sh --iterations 10
#   scripts/stress_subprocess_tests.sh --profile ci   # use the CI profile
#
# Exit codes:
#   0  every iteration passed
#   1  at least one iteration failed (failing log preserved at .stress-logs/run-N.log)
#   2  argument or environment error
#
# This is a developer / CI affordance, not part of `make all`. Run it
# locally before raising the cap further, and wire it into a nightly
# matrix if the project starts caring about long-tail flake rate.

set -euo pipefail

iterations=5
profile="default"
log_dir=".stress-logs"

usage() {
  cat <<'EOF'
Usage: scripts/stress_subprocess_tests.sh [--iterations N] [--profile P]

Loops `cargo nextest run -p harn-cli` N times and reports any flakes.
Logs from each iteration are written to .stress-logs/run-N.log.

Options:
  -n, --iterations N   Number of iterations (default: 5; minimum: 1)
  --profile P          Nextest profile to use (default: default)
  --log-dir DIR        Directory for per-iteration logs (default: .stress-logs)
  -h, --help           Show this help

The script does NOT pre-build; cargo will build incrementally on the
first iteration. Subsequent iterations reuse the warm artifact dir.

Exit code is 0 if every iteration passed, 1 otherwise.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -n|--iterations)
      if [[ $# -lt 2 ]]; then
        echo "error: --iterations requires a value" >&2
        exit 2
      fi
      iterations="$2"
      shift 2
      ;;
    --profile)
      if [[ $# -lt 2 ]]; then
        echo "error: --profile requires a value" >&2
        exit 2
      fi
      profile="$2"
      shift 2
      ;;
    --log-dir)
      if [[ $# -lt 2 ]]; then
        echo "error: --log-dir requires a value" >&2
        exit 2
      fi
      log_dir="$2"
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

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not on PATH" >&2
  exit 2
fi

if ! cargo nextest --version >/dev/null 2>&1; then
  echo "error: cargo-nextest not installed; run 'cargo install cargo-nextest --locked'" >&2
  exit 2
fi

mkdir -p "$log_dir"

failures=0
total_seconds=0
for ((i = 1; i <= iterations; i++)); do
  log_file="$log_dir/run-$i.log"
  printf "[%d/%d] cargo nextest run -p harn-cli --profile %s ... " \
    "$i" "$iterations" "$profile"

  start=$(date +%s)
  if cargo nextest run -p harn-cli --profile "$profile" \
        --no-fail-fast >"$log_file" 2>&1; then
    end=$(date +%s)
    elapsed=$((end - start))
    total_seconds=$((total_seconds + elapsed))
    printf "PASS in %ds\n" "$elapsed"
  else
    end=$(date +%s)
    elapsed=$((end - start))
    total_seconds=$((total_seconds + elapsed))
    printf "FAIL in %ds (log: %s)\n" "$elapsed" "$log_file"
    failures=$((failures + 1))
  fi
done

printf "\n"
printf "Stress summary: %d/%d iterations passed, %ds total wall time.\n" \
  "$((iterations - failures))" "$iterations" "$total_seconds"

if [[ "$failures" -gt 0 ]]; then
  printf "Re-investigate the dyld pre-warm (test_util/process.rs) and the\n"
  printf "harn-subprocess / harn-cli-bin caps in .config/nextest.toml.\n"
  exit 1
fi
