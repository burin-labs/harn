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
if [[ "$1" == "workflow" || ( "$1" == "api" && ( "$2" == "repos/burin-labs/harn/check-runs" || "$2" == repos/burin-labs/harn/issues/*/comments ) ) ]]; then
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

# An unfinished review is not a review. harn#9477 stalled at 180453b0: the
# reviewer posted "did not finish", the census counted that bot review as an
# outcome, and no sweep asked again until the pull request was reopened by
# hand. Each unfinished verdict on a head is asked again once, after it lands;
# the third one stops the sweep and says so once on the pull request.
again="$(fake_sha 0 4)"
in_flight="$(fake_sha 0 5)"
spent="$(fake_sha 0 6)"
told="$(fake_sha 0 7)"
mixed="$(fake_sha 0 8)"
unfinished_review() {
  printf '{"author":{"__typename":"Bot"},"commit":{"oid":"%s"},"submittedAt":"%s","body":"<!-- automated-review-unfinished: %s -->\\n**Automated review did not finish.** This run does not approve."}' "$1" "$2" "$1"
}
exhausted_notice() {
  printf '{"author":{"__typename":"Bot"},"createdAt":"2026-10-06T02:00:00Z","body":"<!-- automated-review-sweep-exhausted: %s -->"}' "$1"
}
FAKE_CENSUS="$(cat <<JSON
{"data":{"repository":{"pullRequests":{"pageInfo":{"hasNextPage":false},"nodes":[
$(pr 10 false "$again" User "$(unfinished_review "$again" 2026-10-06T02:00:00Z)" ""),
$(pr 11 false "$in_flight" User "$(unfinished_review "$in_flight" 2026-10-06T01:00:00Z)" ""),
$(pr 12 false "$spent" User "$(unfinished_review "$spent" 2026-10-06T00:00:00Z),$(unfinished_review "$spent" 2026-10-06T01:00:00Z),$(unfinished_review "$spent" 2026-10-06T02:00:00Z)" ""),
$(pr 13 false "$told" User "$(unfinished_review "$told" 2026-10-06T00:00:00Z),$(unfinished_review "$told" 2026-10-06T01:00:00Z),$(unfinished_review "$told" 2026-10-06T02:00:00Z)" "$(exhausted_notice "$told")"),
$(pr 14 false "$mixed" User "$(unfinished_review "$mixed" 2026-10-06T01:00:00Z),{\"author\":{\"__typename\":\"Bot\"},\"commit\":{\"oid\":\"$mixed\"},\"submittedAt\":\"2026-10-06T02:00:00Z\",\"body\":\"real\"}" "")
]}}}}
JSON
)"
export FAKE_CENSUS
export FAKE_REQUESTS="$again=2026-10-06T01:30:00Z $in_flight=2026-10-06T02:30:00Z $spent=2026-10-06T02:30:00Z $told=2026-10-06T02:30:00Z $mixed=2026-10-06T00:30:00Z"
plan="$("$script" --repo burin-labs/harn --now 2026-10-06T03:00:00Z)"
expected="$(printf '10\t%s\tunfinished\n12\t%s\texhausted' "$again" "$spent")"
[[ "$plan" == "$expected" ]] || fail "unexpected plan for unfinished reviews:
$plan"

: > "$calls"
REVIEW_REPOSITORY=burin-labs/reviewer REVIEW_WORKFLOW=review.yml DISPATCH_TOKEN=token \
  REVIEW_SWEEP_SPACING_SECONDS=0 \
  "$script" --repo burin-labs/harn --now 2026-10-06T03:00:00Z --limit 1 --apply >/dev/null
grep -q "^workflow run review.yml --repo burin-labs/reviewer -f repository=burin-labs/harn -f pr=10 -f head_sha=$again$" "$calls" \
  || fail "the unfinished head was not requested again"
[[ "$(grep -c '^workflow run' "$calls")" == 1 ]] || fail "only the unfinished head is requested"
grep -q "^api repos/burin-labs/harn/issues/12/comments -f body=<!-- automated-review-sweep-exhausted: $spent -->" "$calls" \
  || fail "the exhausted head was not told on its pull request, past the request limit"
! grep -q "issues/1[0134]/comments" "$calls" || fail "a stop notice went to the wrong pull request"

# The bound is a setting, so one more allowed retry asks the spent head again.
plan="$(FAKE_REQUESTS="$spent=2026-10-06T01:30:00Z" REVIEW_SWEEP_MAX_UNFINISHED=4 \
  "$script" --repo burin-labs/harn --now 2026-10-06T03:00:00Z)"
grep -q "^12	$spent	unfinished$" <<<"$plan" || fail "the bound did not follow REVIEW_SWEEP_MAX_UNFINISHED:
$plan"

printf 'review_dispatch_sweep_test: ok\n'
