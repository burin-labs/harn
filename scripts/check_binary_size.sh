#!/usr/bin/env bash
# Build-or-check the shipped harn release binary against a size budget and,
# unless disabled, write a cargo-bloat report for follow-up analysis.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

build_release=1
emit_bloat=1
target="${HARN_RELEASE_TARGET:-}"
harn_bin="${HARN_BIN:-}"
budget_mb="${BINARY_SIZE_BUDGET_MB:-188}"
report_dir="${BINARY_SIZE_REPORT_DIR:-}"

usage() {
  cat <<'EOF'
Usage: scripts/check_binary_size.sh [options]

Build the release harn binary with symbol stripping enabled, assert it stays
below the configured size budget, and emit a cargo-bloat top-crates report.

Options:
  --no-build           Check an already-built release binary
  --target TARGET      Cargo target triple to build/check
  --bin PATH           Override the harn binary path
  --budget-mb MB       Maximum binary size in MiB (default: 188)
  --report-dir DIR     Directory for binary-size and cargo-bloat reports
  --skip-bloat         Only write binary-size.txt; do not run cargo-bloat
  -h, --help           Show this help

Environment:
  BINARY_SIZE_BUDGET_MB   Default budget in MiB
  BINARY_SIZE_REPORT_DIR  Default report directory
  CARGO_TARGET_DIR        Cargo target directory
  HARN_BIN                Default harn binary path
  HARN_RELEASE_TARGET     Default Cargo target triple
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build)
      build_release=0
      shift
      ;;
    --target)
      if [[ $# -lt 2 ]]; then
        echo "error: --target requires a value" >&2
        exit 2
      fi
      target="${2:-}"
      shift 2
      ;;
    --bin)
      if [[ $# -lt 2 ]]; then
        echo "error: --bin requires a path" >&2
        exit 2
      fi
      harn_bin="${2:-}"
      shift 2
      ;;
    --budget-mb)
      if [[ $# -lt 2 ]]; then
        echo "error: --budget-mb requires a value" >&2
        exit 2
      fi
      budget_mb="${2:-}"
      shift 2
      ;;
    --skip-bloat)
      emit_bloat=0
      shift
      ;;
    --report-dir)
      if [[ $# -lt 2 ]]; then
        echo "error: --report-dir requires a path" >&2
        exit 2
      fi
      report_dir="${2:-}"
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

if ! awk -v mb="$budget_mb" 'BEGIN { exit !(mb > 0) }'; then
  echo "error: --budget-mb must be a positive number, got '$budget_mb'" >&2
  exit 2
fi

target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
target_args=()
target_component=""
if [[ -n "$target" ]]; then
  target_args=(--target "$target")
  target_component="$target/"
fi

if [[ -z "$harn_bin" ]]; then
  exe_suffix=""
  case "$target" in
    *windows*|*msvc*) exe_suffix=".exe" ;;
  esac
  harn_bin="$target_dir/${target_component}release/harn${exe_suffix}"
fi

if [[ -z "$report_dir" ]]; then
  report_dir="$target_dir/binary-size/${target:-host}"
fi

budget_bytes="$(awk -v mb="$budget_mb" 'BEGIN { printf "%.0f", mb * 1024 * 1024 }')"

if [[ "$build_release" -eq 1 ]]; then
  export CARGO_PROFILE_RELEASE_STRIP="${CARGO_PROFILE_RELEASE_STRIP:-symbols}"
  cargo build --release -p harn-cli --bin harn "${target_args[@]}"
fi

if [[ ! -f "$harn_bin" ]]; then
  echo "error: harn binary not found at $harn_bin" >&2
  exit 1
fi

actual_bytes="$(wc -c < "$harn_bin" | tr -d '[:space:]')"
actual_mib="$(awk -v bytes="$actual_bytes" 'BEGIN { printf "%.2f", bytes / 1024 / 1024 }')"
budget_mib="$(awk -v bytes="$budget_bytes" 'BEGIN { printf "%.2f", bytes / 1024 / 1024 }')"
size_report="$report_dir/binary-size.txt"
bloat_report="$report_dir/cargo-bloat-crates.txt"

mkdir -p "$report_dir"

{
  echo "harn binary size"
  echo "================"
  echo "binary: $harn_bin"
  echo "target: ${target:-host}"
  echo "bytes: $actual_bytes"
  echo "mib: $actual_mib"
  echo "budget_bytes: $budget_bytes"
  echo "budget_mib: $budget_mib"
} | tee "$size_report"

size_ok=1
if (( actual_bytes > budget_bytes )); then
  size_ok=0
  echo "error: harn binary is ${actual_mib} MiB, above budget ${budget_mib} MiB" >&2
fi

if [[ "$emit_bloat" -eq 1 ]]; then
  if ! command -v cargo-bloat >/dev/null 2>&1; then
    echo "error: cargo-bloat is required to produce $bloat_report" >&2
    exit 2
  fi

  # `cargo bloat` performs its own analysis build and can relink the target
  # binary with different metadata. Preserve the exact stripped binary whose
  # size we checked so the script is safe to run before a packaging step.
  restore_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-binary-size.XXXXXX")"
  checked_binary="$restore_dir/harn"
  cp -p "$harn_bin" "$checked_binary"
  restore_checked_binary() {
    if [[ -f "$checked_binary" ]]; then
      cp -p "$checked_binary" "$harn_bin"
    fi
    rm -rf "$restore_dir"
  }
  trap restore_checked_binary EXIT

  cargo_bloat_target_dir_args=()
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    cargo_bloat_target_dir_args=(--target-dir "$CARGO_TARGET_DIR")
  fi

  cargo bloat \
    --release \
    --crates \
    -p harn-cli \
    --bin harn \
    "${target_args[@]}" \
    "${cargo_bloat_target_dir_args[@]}" \
    -n 40 \
    > "$bloat_report"

  {
    echo
    echo "cargo-bloat report: $bloat_report"
  } | tee -a "$size_report"
else
  {
    echo
    echo "cargo-bloat report: skipped"
  } | tee -a "$size_report"
fi

if [[ "$size_ok" -ne 1 ]]; then
  exit 1
fi
