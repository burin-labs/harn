#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
harn_bin="${HARN_BIN:-}"
if [[ -z "$harn_bin" ]]; then
  harn_bin="$("$repo_root/scripts/harn_bin.sh" --print --no-build)"
fi
export HARN_LLM_CALLS_DISABLED=1

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
tmp_root="$(cd "$tmp_root" && pwd -P)"
notice_root="$tmp_root/notice"
extraction_root="$tmp_root/extraction"
output_root="$tmp_root/output"
mkdir -p "$notice_root" "$extraction_root" "$output_root"
cp "$repo_root/scripts/provider_catalog_notice_fixtures/carried-update.json" \
  "$notice_root/notice.json"
cp "$repo_root/scripts/provider_catalog_notice_fixtures/carried-update.json" \
  "$extraction_root/extraction.json"

notice_path="$notice_root/notice.json"
extraction_path="$extraction_root/extraction.json"
script_path="$repo_root/scripts/provider_catalog_notice.harn"

run_notice() {
  "$harn_bin" run "$@" "$script_path" -- \
    --repo-root "$repo_root" \
    --notice "$notice_path" \
    --extraction "$extraction_path" \
    --output-dir "$output_root"
}

# The pre-fix guide's ungranted external notice must stay denied. This negative
# control proves the test is reaching the sandbox boundary rather than passing
# because confinement was disabled.
if run_notice >"$tmp_root/no-grants.out" 2>"$tmp_root/no-grants.err"; then
  echo "FAIL: external notice was readable without a sandbox grant" >&2
  exit 1
fi
grep -F "HARN-CAP-201" "$tmp_root/no-grants.err" >/dev/null
grep -F "$notice_path" "$tmp_root/no-grants.err" >/dev/null

# A notice grant must not accidentally cover an extraction in another root.
if run_notice --read-only-root "$notice_root" \
  >"$tmp_root/notice-only.out" 2>"$tmp_root/notice-only.err"; then
  echo "FAIL: external extraction was readable without its own sandbox grant" >&2
  exit 1
fi
grep -F "HARN-CAP-201" "$tmp_root/notice-only.err" >/dev/null
grep -F "$extraction_path" "$tmp_root/notice-only.err" >/dev/null

# Read grants must not authorize an external receipt write.
if run_notice \
  --read-only-root "$notice_root" \
  --read-only-root "$extraction_root" \
  >"$tmp_root/read-only.out" 2>"$tmp_root/read-only.err"; then
  echo "FAIL: external receipt was writable through read-only grants" >&2
  exit 1
fi
grep -F "HARN-CAP-201" "$tmp_root/read-only.err" >/dev/null
grep -F "$output_root" "$tmp_root/read-only.err" >/dev/null

# The documented narrow grants must reach the real provider-notice workflow,
# preserve sandboxing, and produce the deterministic receipt without a model call.
run_notice \
  --read-only-root "$notice_root" \
  --read-only-root "$extraction_root" \
  --write-root "$output_root" \
  >"$tmp_root/granted.out" 2>"$tmp_root/granted.err"

grep -F ": patch" "$tmp_root/granted.out" >/dev/null
grep -F "sandbox active; extra write root: $output_root" "$tmp_root/granted.err" >/dev/null
grep -F "extra read-only roots:" "$tmp_root/granted.err" >/dev/null
grep -F "$notice_root" "$tmp_root/granted.err" >/dev/null
grep -F "$extraction_root" "$tmp_root/granted.err" >/dev/null
if grep -F -- "--no-sandbox disables" "$tmp_root/granted.err" >/dev/null; then
  echo "FAIL: narrow provider-notice grants disabled the sandbox" >&2
  exit 1
fi

receipt_count="$(find "$output_root" -maxdepth 1 -type f -name '*.json' | wc -l | tr -d ' ')"
if [[ "$receipt_count" != "1" ]]; then
  echo "FAIL: expected one provider-notice receipt, found $receipt_count" >&2
  exit 1
fi
receipt_path="$(find "$output_root" -maxdepth 1 -type f -name '*.json' -print -quit)"
jq -e '.schema_version == "harn.provider_catalog_notice.v1" and .disposition == "patch"' \
  "$receipt_path" >/dev/null

echo "provider_catalog_notice_sandbox_test: ok"
