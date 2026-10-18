#!/usr/bin/env bash
# Drive dispatch_native_qualification.sh against a fake GitHub: reuse a live or
# passed exact-source run, create the certification ref only when absent, and
# refuse a ref that points elsewhere.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
sha=0123456789abcdef0123456789abcdef01234567

mkdir -p "$tmp/bin"
# FAKE_RUNS: JSON workflow_runs array; FAKE_REF: current ref target (unset: absent).
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
case "$*" in
  "api repos/o/r/actions/workflows/macos-nightly.yml/runs"*)
    jq -n --argjson runs "$FAKE_RUNS" '{workflow_runs: $runs}' | jq "${@: -1}" ;;
  "api repos/o/r/git/ref/heads/"*)
    if [[ -f "$FAKE_REF_FILE" ]]; then jq -n --arg s "$(cat "$FAKE_REF_FILE")" '{object: {sha: $s}}' | jq -r "${@: -1}"; else exit 1; fi ;;
  "api -X POST repos/o/r/git/refs"*) last="${@: -1}"; printf '%s' "${last#sha=}" > "$FAKE_REF_FILE" ;;
  "workflow run"*) ;;
  *) echo "unexpected gh $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/gh"

run() {
  : > "$tmp/gh.log"
  rm -f "$tmp/ref"
  [[ -z "${ref:-}" ]] || printf '%s' "$ref" > "$tmp/ref"
  set +e
  env PATH="$tmp/bin:$PATH" FAKE_GH_LOG="$tmp/gh.log" FAKE_REF_FILE="$tmp/ref" \
    GH_REPO=o/r SOURCE_SHA="$sha" FAKE_RUNS="$1" \
    bash "$root/scripts/ci/dispatch_native_qualification.sh" > "$tmp/out" 2>&1
  status=$?
  set -e
}

ref="" run '[{"id":5,"status":"in_progress","conclusion":null}]'
if [[ $status != 0 ]] || grep -q 'workflow run' "$tmp/gh.log"; then fail "a live run was not reused: $(cat "$tmp/out")"; fi

ref="" run '[{"id":5,"status":"completed","conclusion":"failure"}]'
[[ $status == 0 ]] || fail "a fresh dispatch failed: $(cat "$tmp/out")"
grep -q "^api -X POST repos/o/r/git/refs -f ref=refs/heads/release-certify/$sha -f sha=$sha" "$tmp/gh.log" \
  || fail "the certification ref was not created"
grep -q "^workflow run macos-nightly.yml --repo o/r --ref release-certify/$sha" "$tmp/gh.log" \
  || fail "the qualification was not dispatched on the certification ref"

ref="$sha" run '[]'
if [[ $status != 0 ]] || grep -q 'POST' "$tmp/gh.log" || ! grep -q '^workflow run' "$tmp/gh.log"; then
  fail "an existing exact ref was recreated or not dispatched"
fi

ref=1111111111111111111111111111111111111111 run '[]'
if [[ $status == 0 ]] || grep -q '^workflow run' "$tmp/gh.log"; then fail "a conflicting certification ref was used"; fi

echo "dispatch_native_qualification_test: ok"
