# shellcheck shell=bash
# Readers of the protected `green` branch: the newest main commit whose full CI
# suite passed. scripts/ci/green_checkpoint.sh is its only writer.
#
# Every reader fails on an answer it cannot read; none treats it as "no green".

# shellcheck disable=SC2034 # constants are consumed by sourcing scripts
GREEN_CHECKPOINT_SCHEMA=harn.green_checkpoint.v1

# Print green's commit, or an empty line while the branch does not exist yet.
green_tip() {
  local repo=$1 refs
  refs=$(gh api "repos/$repo/git/matching-refs/heads/green") || return 1
  # The endpoint matches by prefix, so `green-foo` must not answer for `green`.
  jq -r 'if type != "array" then error("not a ref list") else . end
    | [.[] | select(.ref == "refs/heads/green")]
    | if length == 0 then ""
      elif length == 1 and (.[0].object.sha | type == "string" and test("^[0-9a-f]{40}$"))
      then .[0].object.sha
      else error("malformed green ref") end' <<< "$refs"
}

# Print how HEAD relates to BASE (ahead, behind, identical or diverged).
green_compare() {
  local repo=$1 base=$2 head=$3 body status
  body=$(gh api "repos/$repo/compare/$base...$head?per_page=1") || return 1
  status=$(jq -r '.status' <<< "$body") || return 1
  case "$status" in
    ahead | behind | identical | diverged) printf '%s\n' "$status" ;;
    *) return 1 ;;
  esac
}

# Succeed when green contains SHA, fail with status 1 when it does not, and with
# status 2 when green is absent or the relation cannot be read.
green_contains() {
  local repo=$1 sha=$2 tip relation
  tip=$(green_tip "$repo") || return 2
  [[ -n "$tip" ]] || return 2
  relation=$(green_compare "$repo" "$tip" "$sha") || return 2
  case "$relation" in
    behind | identical) return 0 ;;
    *) return 1 ;;
  esac
}

# Wait until green contains SHA. A release commit's candidate run can finish
# before that commit's own CI run, so the wait is bounded by CI's budget.
green_wait_for() {
  local repo=$1 sha=$2 status deadline
  deadline=$(( $(date +%s) + ${GREEN_WAIT_SECONDS:-5400} ))
  until green_contains "$repo" "$sha"; do
    status=$?
    if (( status != 1 )); then
      echo "::error::Green is absent or unreadable; refusing to promote uncertified $sha." >&2
      return 1
    fi
    if (( $(date +%s) >= deadline )); then
      echo "::error::$sha is not contained in green: its full CI suite has not passed." >&2
      return 1
    fi
    sleep "${GREEN_POLL_SECONDS:-60}"
  done
}
