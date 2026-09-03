#!/usr/bin/env bash
# Both arms are required. A scanner that flags everything and a scanner that
# flags nothing both look "green" from one direction only, and this gate's
# failure mode is silence, so the clean-comment arm is the load-bearing one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-comment-privacy-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT

# A synthetic repo so the real denylist is never needed and the test never has
# to carry a real forbidden token in its own source.
repo="$fixture_root/repo"
mkdir -p "$repo/scripts"
cp "$root/scripts/check_issue_comment_privacy.sh" "$repo/scripts/"
cp "$root/scripts/check_public_product_names.sh" "$repo/scripts/"
cp "$root/scripts/scan_hashed_denylist.mjs" "$repo/scripts/"

denied_token="synthetic.private"
printf '%s' "$denied_token" | shasum -a 256 | awk '{print $1}' \
  >"$repo/scripts/consumer-host-denylist.sha256"

run_case() {
  local name="$1" json="$2"
  local event="$fixture_root/$name.json"
  printf '%s' "$json" >"$event"
  set +e
  OUT="$("$repo/scripts/check_issue_comment_privacy.sh" "$event" 2>&1)"
  STATUS=$?
  set -e
}

# Arm 1: a comment carrying a forbidden token is flagged.
run_case "dirty" "$(printf '{"comment":{"body":"internal route: %s"}}' "$denied_token")"
if [[ "$STATUS" -ne 1 ]]; then
  echo "FAIL: a comment naming a denied host must exit 1, got $STATUS" >&2
  echo "$OUT" >&2
  exit 1
fi
if grep -qF "$denied_token" <<<"$OUT"; then
  echo "FAIL: the scanner echoed the denied token into its own output" >&2
  exit 1
fi

# Arm 2: a clean comment passes. Without this, a scanner that failed everything
# would satisfy arm 1 and still be worthless.
run_case "clean" '{"comment":{"body":"This reads fine and names nobody."}}'
if [[ "$STATUS" -ne 0 ]]; then
  echo "FAIL: a clean comment must exit 0, got $STATUS" >&2
  echo "$OUT" >&2
  exit 1
fi
if [[ "$OUT" != *"sources=1 violations=0"* ]]; then
  echo "FAIL: a clean comment must report exactly one scanned source, got: $OUT" >&2
  exit 1
fi

# Arm 3: an issue body is scanned too, not only the comment.
run_case "issue-body" "$(printf '{"issue":{"title":"t","body":"host %s"}}' "$denied_token")"
if [[ "$STATUS" -ne 1 ]]; then
  echo "FAIL: an issue body naming a denied host must exit 1, got $STATUS" >&2
  exit 1
fi

# Arm 4: an event this reader cannot parse must be "unmeasured", never a pass.
# A gate that scans zero sources and exits 0 is the defect, not the fix.
run_case "empty" '{"action":"created"}'
if [[ "$STATUS" -ne 2 ]]; then
  echo "FAIL: an unreadable event must exit 2 (unmeasured), got $STATUS" >&2
  echo "$OUT" >&2
  exit 1
fi
if [[ "$OUT" != *"nothing was scanned"* ]]; then
  echo "FAIL: an unreadable event must say nothing was scanned, got: $OUT" >&2
  exit 1
fi

echo "check_issue_comment_privacy tests passed"
