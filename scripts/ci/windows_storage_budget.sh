#!/usr/bin/env bash
# Emit one machine-readable storage receipt for the native Windows build.
# Git Bash provides the same df/du interface used here on hosted and paid
# Windows runners; keeping the owner in shell also makes the contract locally
# falsifiable without a Windows-only PowerShell parser.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/ci/cache_policy.sh
source "${SCRIPT_DIR}/cache_policy.sh"

usage() {
  cat <<'EOF'
usage:
  scripts/ci/windows_storage_budget.sh report PHASE [--target-dir DIR]
                                              [--require-free-headroom]

PHASE is a lowercase receipt label such as initial, after_restore, or terminal.
The command reports filesystem capacity plus Cargo target/home footprints and
appends the same receipt to GITHUB_STEP_SUMMARY when available.

--require-free-headroom turns the reading into a gate: after emitting the
receipt the command fails when free bytes are below
windows_workspace_warm.build_headroom_bytes in .github/cache-policy.json. A
build entered below that floor cannot link the workspace, so the useful failure
is here with the measurement in the message rather than an ENOSPC thousands of
lines into the compile. The floor has exactly one owner, that JSON document.
EOF
}

directory_bytes_or_zero() {
  local path=$1
  if [[ -z "$path" || ! -d "$path" ]]; then
    printf '0\n'
    return 0
  fi
  local kib
  kib="$(du -sk "$path" | awk '{ print $1 }')"
  [[ "$kib" =~ ^[0-9]+$ ]] || {
    echo "error: could not measure directory bytes for $path" >&2
    return 1
  }
  printf '%s\n' "$((kib * 1024))"
}

report() {
  local phase=${1:-}
  shift || true
  local target_dir="${CARGO_TARGET_DIR:-target}"
  local require_headroom=0
  while (($#)); do
    case "$1" in
      --target-dir) target_dir="${2:-}"; shift 2 ;;
      --require-free-headroom) require_headroom=1; shift ;;
      -h|--help) usage; return 0 ;;
      *) echo "error: unknown argument: $1" >&2; usage >&2; return 2 ;;
    esac
  done
  if [[ ! "$phase" =~ ^[a-z][a-z0-9_]*$ ]]; then
    echo "error: PHASE must match ^[a-z][a-z0-9_]*$" >&2
    return 2
  fi

  local probe=$target_dir
  while [[ ! -e "$probe" ]]; do
    local parent
    parent="$(dirname "$probe")"
    if [[ "$parent" == "$probe" ]]; then
      probe=.
      break
    fi
    probe=$parent
  done

  local df_line total_kib used_kib free_kib
  df_line="$(df -Pk "$probe" | awk 'NR > 1 { line = $0 } END { print line }')"
  read -r _ total_kib used_kib free_kib _ <<<"$df_line"
  if [[ ! "$total_kib" =~ ^[0-9]+$ || ! "$used_kib" =~ ^[0-9]+$ || ! "$free_kib" =~ ^[0-9]+$ ]]; then
    echo "error: could not measure filesystem capacity for $probe" >&2
    return 1
  fi

  local cargo_home="${CARGO_HOME:-}"
  if [[ -z "$cargo_home" && -n "${HOME:-}" ]]; then
    cargo_home="$HOME/.cargo"
  fi
  local target_bytes cargo_home_bytes total_bytes used_bytes free_bytes
  target_bytes="$(directory_bytes_or_zero "$target_dir")"
  cargo_home_bytes="$(directory_bytes_or_zero "$cargo_home")"
  total_bytes="$((total_kib * 1024))"
  used_bytes="$((used_kib * 1024))"
  free_bytes="$((free_kib * 1024))"

  local receipt
  receipt="windows_storage_phase=${phase} windows_storage_total_bytes=${total_bytes} windows_storage_used_bytes=${used_bytes} windows_storage_free_bytes=${free_bytes} windows_storage_target_bytes=${target_bytes} windows_storage_cargo_home_bytes=${cargo_home_bytes}"
  printf '%s\n' "$receipt"
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "### Windows storage: ${phase}"
      echo
      echo "\`${receipt}\`"
    } >> "$GITHUB_STEP_SUMMARY"
  fi

  # The receipt is emitted first on purpose: a run that trips the floor must
  # still publish the same measurement an ordinary run publishes, so the
  # failure and the healthy baseline are read the same way.
  if ((require_headroom)); then
    local floor_bytes
    floor_bytes="$(harn_cache_policy_jq '.windows_workspace_warm.build_headroom_bytes')"
    if [[ ! "$floor_bytes" =~ ^[0-9]+$ ]]; then
      echo "error: cache-policy.json windows_workspace_warm.build_headroom_bytes is not an integer" >&2
      return 1
    fi
    if ((free_bytes < floor_bytes)); then
      echo "error: ${phase} free space ${free_bytes} bytes is below the ${floor_bytes}-byte build headroom floor on the filesystem holding ${target_dir}; the workspace link cannot complete from here" >&2
      return 1
    fi
  fi
}

main() {
  local command=${1:-}
  shift || true
  case "$command" in
    report) report "$@" ;;
    -h|--help) usage ;;
    *) echo "error: unknown command: ${command:-<missing>}" >&2; usage >&2; return 2 ;;
  esac
}

main "$@"
