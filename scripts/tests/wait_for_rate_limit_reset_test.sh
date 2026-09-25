#!/usr/bin/env bash
# A failed attestation in a release candidate runs once more only when the
# token's API budget is spent, and only after the window resets.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
script="$root/scripts/ci/wait_for_rate_limit_reset.sh"

fail() {
  echo "wait_for_rate_limit_reset_test: $*" >&2
  exit 1
}

# `gh api rate_limit` answers from $tmp/rate_limit; a missing file is an API
# failure. `sleep` records what it was asked to wait instead of waiting.
mkdir -p "$tmp/bin"
cat > "$tmp/bin/gh" <<EOF
#!/usr/bin/env bash
[[ "\$1 \$2" == "api rate_limit" ]] || { echo "unexpected gh call: \$*" >&2; exit 97; }
[[ -f "$tmp/rate_limit" ]] || { echo "HTTP 502" >&2; exit 1; }
cat "$tmp/rate_limit"
EOF
cat > "$tmp/bin/sleep" <<EOF
#!/usr/bin/env bash
echo "\$1" > "$tmp/slept"
EOF
chmod +x "$tmp/bin/gh" "$tmp/bin/sleep"

run() {
  rm -f "$tmp/slept"
  PATH="$tmp/bin:$PATH" "$script" "Attest release files" > "$tmp/out" 2>&1
}

now="$(date +%s)"

# Spent budget, reset in 60s: wait for the reset, then allow the second attempt.
echo "1000 0 $((now + 60))" > "$tmp/rate_limit"
run || fail "a spent budget was refused: $(cat "$tmp/out")"
slept="$(cat "$tmp/slept" 2>/dev/null || echo none)"
if [[ ! "$slept" =~ ^[0-9]+$ ]] || (( slept < 60 || slept > 70 )); then
  fail "expected a wait of about 65s for the reset, got '$slept'"
fi
grep -Fq "hit the API rate limit (0 of 1000 left)" "$tmp/out" || fail "the wait does not say why: $(cat "$tmp/out")"

# Budget left: the step failed for another reason, so it is not retried.
echo "1000 800 $((now + 60))" > "$tmp/rate_limit"
if run; then fail "a failure with budget left was retried"; fi
[[ ! -f "$tmp/slept" ]] || fail "refused retry still waited"
grep -Fq "800 of 1000 API requests left, so the failure was not the rate limit" "$tmp/out" \
  || fail "the refusal does not name the budget: $(cat "$tmp/out")"

# Reset beyond the cap: refused rather than holding the job indefinitely.
echo "1000 0 $((now + 7200))" > "$tmp/rate_limit"
if run; then fail "a reset beyond the cap was waited for"; fi
grep -Fq "beyond the 3900s cap" "$tmp/out" || fail "the cap refusal does not say so: $(cat "$tmp/out")"

# The budget cannot be read: refused.
rm -f "$tmp/rate_limit"
if run; then fail "an unreadable budget was retried"; fi
grep -Fq "could not be read" "$tmp/out" || fail "the read failure does not say so: $(cat "$tmp/out")"

# A malformed answer is refused, not treated as a spent budget.
echo "unexpected" > "$tmp/rate_limit"
if run; then fail "a malformed /rate_limit answer was retried"; fi

echo "wait_for_rate_limit_reset_test: ok"
