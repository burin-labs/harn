#!/usr/bin/env bash
# The check passes only for a success recorded on the exact SHA; every other state is a failure.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"

sha="1111111111111111111111111111111111111111"
other="2222222222222222222222222222222222222222"
name="macOS workspace tests (owned)"

# The stub answers the check-runs call from the fixture in $FIXTURE, run through the real --jq filter.
cat > "$tmp/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
filter=""
while [[ $# -gt 0 ]]; do
  [[ "$1" == --jq ]] && filter="$2"
  shift
done
jq -c "$filter" "$FIXTURE"
SH
chmod +x "$tmp/bin/gh"

decide() {
  FIXTURE="$tmp/fixture.json" PATH="$tmp/bin:$PATH" "$root/scripts/ci/owned_macos_pr_check.sh" burin-labs/harn "$sha" >/dev/null 2>&1 && echo pass || echo fail
}
fixture() { printf '%s\n' "$1" > "$tmp/fixture.json"; }
run() { printf '{"check_runs":[{"id":%s,"name":"%s","head_sha":"%s","status":"%s","conclusion":%s}]}' "$1" "$2" "$3" "$4" "$5"; }
expect() {
  [[ "$(decide)" == "$1" ]] || { echo "expected $1: $2" >&2; exit 1; }
}

fixture "$(run 1 "$name" "$sha" completed '"success"')"
expect pass "a success on the exact SHA"

# Negative controls: each of these must not read as success.
fixture '{"check_runs":[]}'
expect fail "no check at all"
fixture "$(run 1 "macOS workspace tests" "$sha" completed '"success"')"
expect fail "a differently named check (the hosted one)"
fixture "$(run 1 "$name" "$other" completed '"success"')"
expect fail "a success on another SHA"
fixture "$(run 1 "$name" "$sha" in_progress null)"
expect fail "a check still running"
fixture "$(run 1 "$name" "$sha" completed '"failure"')"
expect fail "a failure"
fixture "$(run 1 "$name" "$sha" completed '"neutral"')"
expect fail "a neutral conclusion"
fixture '{"check_runs":[{"id":1,"name":"'"$name"'","head_sha":"'"$sha"'","status":"completed","conclusion":"success"},{"id":2,"name":"'"$name"'","head_sha":"'"$sha"'","status":"completed","conclusion":"failure"}]}'
expect fail "the newest run failed after an older success"

if FIXTURE="$tmp/fixture.json" PATH="$tmp/bin:$PATH" "$root/scripts/ci/owned_macos_pr_check.sh" burin-labs/harn main >/dev/null 2>&1; then
  echo "a branch name must not be accepted as a SHA" >&2
  exit 1
fi
echo "owned macOS PR check: ok"
