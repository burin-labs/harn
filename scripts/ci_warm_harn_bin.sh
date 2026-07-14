#!/usr/bin/env bash
set -euo pipefail

github_env="${GITHUB_ENV:-}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/cargo_env.sh
source "$script_dir/lib/cargo_env.sh"
# shellcheck source=scripts/lib/harn_bin.sh
source "$script_dir/lib/harn_bin.sh"

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

if [[ -n "${HARN_BIN:-}" ]]; then
  harn_require_executable_bin "$HARN_BIN"
else
  echo "=== Warming Harn CLI binary ==="
  harn_export_cargo_build_dir_for_target "${CARGO_TARGET_DIR:-}" || true
  HARN_BIN="$(cargo run --quiet --bin harn -- "$(harn_internal_executable_path_command)")"
  harn_require_executable_bin "$HARN_BIN"
fi

export HARN_BIN
printf 'ok: harn-bin (%s)\n' "$HARN_BIN"

if [[ -n "$github_env" ]]; then
  printf 'HARN_BIN=%s\n' "$HARN_BIN" >> "$github_env"
fi
