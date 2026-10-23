#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
if [[ "${1:-}" == --print-inputs ]]; then
  cat "$root/scripts/docs_site_inputs.txt"
elif [[ -n "${HARN_BIN:-}" ]]; then
  exec "$HARN_BIN" run --no-sandbox "$root/scripts/build_docs_site.harn" -- "$@"
else
  exec "$root/scripts/bootstrap-harn.sh" --seed-command run --no-sandbox \
    "$root/scripts/build_docs_site.harn" -- "$@"
fi
