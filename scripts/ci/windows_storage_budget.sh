#!/usr/bin/env bash
# Emit one machine-readable storage receipt for the native Windows build.
# Git Bash provides the same df/du interface used here on hosted and paid
# Windows runners; keeping the owner in shell also makes the contract locally
# falsifiable without a Windows-only PowerShell parser.
set -euo pipefail

usage() {
  cat <<'EOF'
usage:
  scripts/ci/windows_storage_budget.sh report PHASE [--target-dir DIR]

PHASE is a lowercase receipt label such as initial, after_restore, or terminal.
The command reports filesystem capacity plus Cargo target/home footprints and
appends the same receipt to GITHUB_STEP_SUMMARY when available.
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
  while (($#)); do
    case "$1" in
      --target-dir) target_dir="${2:-}"; shift 2 ;;
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
