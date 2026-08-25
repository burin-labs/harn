#!/usr/bin/env bash
set -euo pipefail

: "${HARN_BIN:?set HARN_BIN to the warmed Harn executable}"

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-connector-scaffold.XXXXXX")"
trap 'rm -rf "$tmp_root"' EXIT
receipt="$tmp_root/package-verify.json"

(
  cd "$tmp_root"
  HARN_LLM_PROVIDER=mock HARN_LLM_CALLS_DISABLED=1 \
    "$HARN_BIN" new connector example-connector >/dev/null
  cd example-connector
  HARN_LLM_PROVIDER=mock HARN_LLM_CALLS_DISABLED=1 \
    "$HARN_BIN" package verify . --strict --json >"$receipt"
)

jq -e '
  .ok == true
  and .data.strict_requested == true
  and any(
    .data.checks[];
    .name == "harn check" and .reached == true and .status == "pass"
  )
  and any(
    .data.checks[];
    .name == "connector contract" and .reached == true and .status == "pass"
  )
' "$receipt" >/dev/null

echo "connector_scaffold_strict_package_test: ok"
