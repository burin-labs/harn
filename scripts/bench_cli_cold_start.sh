#!/usr/bin/env bash
# Cold-start budget gate for ported CLI subcommands.
#
# Times each tracked `harn` subcommand from process start to exit, with
# the bytecode cache wiped between runs so every invocation pays the
# full dispatch + parse + typecheck + compile cost. Compares medians
# against:
#
#   - the per-command budget in perf/cli/budgets.toml, and
#   - the most recent recorded baseline in perf/cli/baselines/main.json.
#
# Either condition can fail the gate (see the README for the rules).
#
# Hyperfine is used when present on $PATH; otherwise the Harn controller
# falls back to `monotonic_ms()` around subprocess invocation. The
# fallback is intentional — `hyperfine` is not in the default dev-setup
# install on macOS.
#
# See perf/cli/README.md for usage and design notes. Tracks epic #2293
# (G5 = #2298).

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
budgets_file="${HARN_CLI_BUDGETS:-$repo_root/perf/cli/budgets.toml}"
baseline_file="${HARN_CLI_BASELINE:-$repo_root/perf/cli/baselines/main.json}"
iterations="${HARN_CLI_BENCH_ITERATIONS:-20}"
build_release=1
harn_bin="${HARN_BIN:-}"
commands_filter=""
update_baseline=0

usage() {
  cat <<'EOF'
Usage: scripts/bench_cli_cold_start.sh [options]

Time each tracked `harn` subcommand under cold-start conditions and
compare against the per-command budget plus the most recent recorded
baseline. Exits non-zero if any command fails the gate.

Options:
  -n, --iterations N      Timed runs per command (default: 20)
  --no-build              Skip `cargo build --release --bin harn`
  --budgets FILE          Override perf/cli/budgets.toml
  --baseline FILE         Override perf/cli/baselines/main.json
  --commands NAMES        Comma-separated subset of bench keys to run
  --update-baseline       Overwrite this commit's slot in the baseline file
                          even if it was already populated
  -h, --help              Show this help

Environment:
  HARN_BIN                  Override the harn binary path
  HARN_CLI_BENCH_ITERATIONS Default iteration count
  HARN_CLI_BUDGETS          Default budgets.toml path
  HARN_CLI_BASELINE         Default baseline JSON path
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
    --no-build)
      build_release=0
      shift
      ;;
    --budgets)
      if [[ $# -lt 2 ]]; then
        echo "error: --budgets requires a path" >&2
        exit 2
      fi
      budgets_file="${2:-}"
      shift 2
      ;;
    --baseline)
      if [[ $# -lt 2 ]]; then
        echo "error: --baseline requires a path" >&2
        exit 2
      fi
      baseline_file="${2:-}"
      shift 2
      ;;
    --commands)
      if [[ $# -lt 2 ]]; then
        echo "error: --commands requires a value" >&2
        exit 2
      fi
      commands_filter="${2:-}"
      shift 2
      ;;
    --update-baseline)
      update_baseline=1
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

if [[ ! -f "$budgets_file" ]]; then
  echo "error: budgets file not found: $budgets_file" >&2
  exit 2
fi

if [[ ! -f "$baseline_file" ]]; then
  echo "error: baseline file not found: $baseline_file" >&2
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

# Hand off to the Harn controller. It owns:
#   - TOML parse of budgets
#   - JSON load/dump of baselines
#   - dispatch between hyperfine and the monotonic fallback
#   - cache-wipe orchestration around each run
#   - budget + baseline comparison and pass/fail decision
HARN_BIN="$harn_bin" \
HARN_CLI_BUDGETS_FILE="$budgets_file" \
HARN_CLI_BASELINE_FILE="$baseline_file" \
HARN_CLI_ITERATIONS="$iterations" \
HARN_CLI_COMMANDS_FILTER="$commands_filter" \
HARN_CLI_UPDATE_BASELINE="$update_baseline" \
HARN_CLI_REPO_ROOT="$repo_root" \
  exec "$harn_bin" run --no-sandbox "$repo_root/scripts/bench_cli_cold_start.harn"
