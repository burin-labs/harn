#!/usr/bin/env bash
set -euo pipefail

github_env="${GITHUB_ENV:-}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/cargo_env.sh
source "$script_dir/lib/cargo_env.sh"

usage() {
  cat <<'EOF'
usage: scripts/ci_warm_harn_bin.sh [--github-env PATH]

Builds or validates one workspace debug `harn` binary, exports it as HARN_BIN
for the current process, and optionally appends HARN_BIN=... to a GitHub Actions
environment file for subsequent steps.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --github-env)
      if [[ $# -lt 2 ]]; then
        echo "error: --github-env requires a path" >&2
        exit 2
      fi
      github_env="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unexpected argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

debug_harn_binary() {
  local target_dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$target_dir" ]]; then
    target_dir="$(cargo metadata --format-version=1 --no-deps \
      | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  fi

  local suffix=""
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) suffix=".exe" ;;
  esac
  printf '%s/debug/harn%s\n' "$target_dir" "$suffix"
}

if [[ -n "${HARN_BIN:-}" ]]; then
  if [[ ! -x "$HARN_BIN" ]]; then
    echo "error: HARN_BIN is not executable: $HARN_BIN" >&2
    exit 1
  fi
else
  echo "=== Warming Harn CLI binary ==="
  harn_export_cargo_build_dir_under_target "${CARGO_TARGET_DIR:-}" || true
  cargo build --quiet --bin harn
  HARN_BIN="$(debug_harn_binary)"
  if [[ ! -x "$HARN_BIN" ]]; then
    echo "error: warm build completed but HARN_BIN is not executable: $HARN_BIN" >&2
    exit 1
  fi
fi

export HARN_BIN
printf 'ok: harn-bin (%s)\n' "$HARN_BIN"

if [[ -n "$github_env" ]]; then
  printf 'HARN_BIN=%s\n' "$HARN_BIN" >> "$github_env"
fi
