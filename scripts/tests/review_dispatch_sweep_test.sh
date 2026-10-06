#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/review_dispatch_sweep.sh"
fixture_root="$(mktemp -d)"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/bin"
calls="$fixture_root/calls"

fake_sha() { printf '%040d' "$1" | tr '0' "$2"; }
capped="$(fake_sha 0 a)"
lost="$(fake_sha 0 b)"
recent="$(fake_sha 0 c)"
reviewed="$(fake_sha 0 d)"
decided="$(fake_sha 0 e)"
capped_today="$(fake_sha 0 f)"
draft="$(fake_sha 0 1)"
bot="$(fake_sha 0 2)"
unrequested="$(fake_sha 0 3)"

# shellcheck disable=SC2016 # Markdown backticks, not command substitution.
stand_down() {
  printf '{"author":{"__typename":"Bot"},"createdAt":"%s","body":"<!-- automated-review-stand-down: %s -->\\n**No automated review** (`%s`): reason."}' "$1" "$2" "$3"
}
pr() {
  printf '{"number":%s,"isDraft":%s,"isCrossRepository":false,"headRefOid":"%s","author":{"__typename":"%s"},"reviews":{"nodes":[%s]},"comments":{"nodes":[%s]}}' "$@"
}

FAKE_CENSUS="$(cat <<JSON
{"data":{"repository":{"pullRequests":{"pageInfo":{"hasNextPage":false},"nodes":[
$(pr 1 false "$capped" User "" "$(stand_down 2026-10-05T15:00:00Z "$capped" daily_cap)"),
$(pr 2 false "$lost" User "" ""),
$(pr 3 false "$recent" User "" ""),
$(pr 4 false "$reviewed" User "{\"author\":{\"__typename\":\"Bot\"},\"commit\":{\"oid\":\"$reviewed\"}}" ""),
$(pr 5 false "$decided" User "" "$(stand_down 2026-10-05T15:00:00Z "$decided" generated_only)"),
$(pr 6 false "$capped_today" User "" "$(stand_down 2026-10-05T15:00:00Z "$capped_today" daily_cap)"),
$(pr 7 true "$draft" User "" ""),
$(pr 8 false "$bot" Bot "" ""),
$(pr 9 false "$unrequested" User "" "")
]}}}}
JSON
)"
export FAKE_CENSUS FAKE_GH_CALLS="$calls"
export FAKE_REQUESTS="$capped=2026-10-05T14:00:00Z $lost=2026-10-06T00:14:00Z $recent=2026-10-06T02:30:00Z $capped_today=2026-10-06T01:00:00Z"

cat > "$fixture_root/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_GH_CALLS"
if [[ "$1" == "api" && "$2" == "graphql" ]]; then
  [[ "${FAKE_CENSUS_FAILURE:-0}" != "1" ]] || exit 1
  printf '%s\n' "$FAKE_CENSUS"
  exit 0
fi
if [[ "$1" == "api" && "$2" == repos/burin-labs/harn/commits/*/check-runs* ]]; then
  sha="${2#repos/burin-labs/harn/commits/}"
  sha="${sha%%/*}"
  for pair in $FAKE_REQUESTS; do
    if [[ "${pair%%=*}" == "$sha" ]]; then
      printf '%s\n' "${pair#*=}"
      exit 0
    fi
  done
  printf 'null\n'
  exit 0
fi
if [[ "$1" == "workflow" || ( "$1" == "api" && "$2" == "repos/burin-labs/harn/check-runs" ) ]]; then
  exit 0
fi
printf 'unexpected gh call: %s\n' "$*" >&2
exit 1
SH
chmod +x "$fixture_root/bin/gh"
export PATH="$fixture_root/bin:$PATH"

fail() {
  printf 'review_dispatch_sweep_test: %s\n' "$*" >&2
  exit 1
}

# The plan: a head capped yesterday, a head whose request vanished two hours
# ago, and a head never requested. Nothing else.
plan="$("$script" --repo burin-labs/harn --now 2026-10-06T03:00:00Z)"
expected="$(printf '1\t%s\tdaily_cap\n2\t%s\tno_outcome\n9\t%s\tno_outcome' "$capped" "$lost" "$unrequested")"
[[ "$plan" == "$expected" ]] || fail "unexpected plan:
$plan"
! grep -q '^workflow' "$calls" || fail "a read-only plan dispatched a review"

# Applying sends at most --limit requests, oldest first, each with its record.
: > "$calls"
REVIEW_REPOSITORY=burin-labs/reviewer REVIEW_WORKFLOW=review.yml DISPATCH_TOKEN=token \
  REVIEW_SWEEP_SPACING_SECONDS=0 \
  "$script" --repo burin-labs/harn --now 2026-10-06T03:00:00Z --limit 2 --apply >/dev/null
[[ "$(grep -c '^workflow run review.yml' "$calls")" == 2 ]] || fail "expected two dispatches"
grep -q "^workflow run review.yml --repo burin-labs/reviewer -f repository=burin-labs/harn -f pr=1 -f head_sha=$capped$" "$calls" \
  || fail "the capped head was not requested first"
grep -q "^workflow run review.yml --repo burin-labs/reviewer -f repository=burin-labs/harn -f pr=2 -f head_sha=$lost$" "$calls" \
  || fail "the lost head was not requested second"
[[ "$(grep -c '^api repos/burin-labs/harn/check-runs' "$calls")" == 2 ]] || fail "each request must be recorded"
! grep -q "pr=9 " "$calls" || fail "the limit was exceeded"

# Same day, an hour after the lost head was requested again: nothing repeats.
export FAKE_REQUESTS="$capped=2026-10-06T03:00:00Z $lost=2026-10-06T03:01:00Z $recent=2026-10-06T02:30:00Z $capped_today=2026-10-06T01:00:00Z $unrequested=2026-10-06T03:02:00Z"
plan="$("$script" --repo burin-labs/harn --now 2026-10-06T04:00:00Z)"
[[ "$plan" == "No pull request head is waiting for an automatic review outcome." ]] || fail "a request repeated within its window:
$plan"

# An unreadable census refuses rather than reporting nothing to do.
if FAKE_CENSUS_FAILURE=1 "$script" --repo burin-labs/harn --now 2026-10-06T03:00:00Z >/dev/null 2>&1; then
  fail "an unreadable census passed"
fi

# The event path records its request as the dispatch job's check run, under
# the same name the sweep reads. A rename on either side would make every
# freshly requested head look unrequested and request it again at once.
workflow="$repo_root/.github/workflows/review-dispatch.yml"
grep -qF "|| 'Automatic review: requested' }}" "$workflow" \
  || fail "the dispatch job no longer reports as the request record"
grep -qxF 'request_check="Automatic review: requested"' "$script" \
  || fail "the sweep no longer reads the request record"

printf 'review_dispatch_sweep_test: ok\n'
