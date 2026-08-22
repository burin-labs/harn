#!/usr/bin/env bash
set -euo pipefail

github_env="${GITHUB_ENV:-}"
cargo_profile="dev"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/cargo_env.sh
source "$script_dir/lib/cargo_env.sh"
# shellcheck source=scripts/lib/harn_bin.sh
source "$script_dir/lib/harn_bin.sh"

usage() {
  cat <<'EOF'
usage: scripts/ci_warm_harn_bin.sh [--profile dev|test] [--github-env PATH]

Builds or validates one workspace debug `harn` binary through the selected
Cargo profile, publishes its exact freshness receipt, exports it as HARN_BIN
for the current process, and optionally appends HARN_BIN=... to a GitHub Actions
environment file for subsequent steps. A receipt-backed producer also appends
HARN_BUILD_FRESHNESS_ID so later Cargo commands preserve that exact artifact.
The dev and test profiles both publish to Cargo's debug output directory;
selecting the producer's real profile avoids recompiling its dependency graph
during provenance convergence.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile)
      if [[ $# -lt 2 ]]; then
        echo "error: --profile requires dev or test" >&2
        exit 2
      fi
      cargo_profile="$2"
      shift 2
      ;;
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

case "$cargo_profile" in
  dev|test) ;;
  *)
    echo "error: --profile must be dev or test" >&2
    exit 2
    ;;
esac

if [[ -n "${HARN_BIN:-}" ]]; then
  harn_require_executable_bin "$HARN_BIN"
else
  echo "=== Warming Harn CLI binary ==="
  # Resolve through the owning wrapper so the build receives and converges the
  # exact compiled provenance identity before a receipt is published.
  HARN_BIN="$(harn_resolve_binary build "$cargo_profile" locked)"
fi

export HARN_BIN
printf 'ok: harn-bin (%s)\n' "$HARN_BIN"
receipt="$(harn_binary_freshness_receipt_path "$HARN_BIN")"
if [[ -r "$receipt" ]]; then
  HARN_BUILD_FRESHNESS_ID="$(harn_verified_build_freshness_id "$HARN_BIN")"
  export HARN_BUILD_FRESHNESS_ID
  printf 'ok: harn-bin-receipt (%s)\n' "$HARN_BUILD_FRESHNESS_ID"
fi

if [[ -n "$github_env" ]]; then
  printf 'HARN_BIN=%s\n' "$HARN_BIN" >> "$github_env"
  if [[ -n "${HARN_BUILD_FRESHNESS_ID:-}" ]]; then
    printf 'HARN_BUILD_FRESHNESS_ID=%s\n' "$HARN_BUILD_FRESHNESS_ID" >> "$github_env"
  fi
fi
