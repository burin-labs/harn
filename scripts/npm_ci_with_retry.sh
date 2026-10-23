#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
if [[ -n "${HARN_BIN:-}" ]]; then
  exec "$HARN_BIN" run --no-sandbox "$root/scripts/npm_ci_with_retry.harn" -- "$@"
fi
exec "$root/scripts/bootstrap-harn.sh" --seed-command run --no-sandbox \
  "$root/scripts/npm_ci_with_retry.harn" -- "$@"
