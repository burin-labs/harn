#!/usr/bin/env bash
# Drive release_candidate_reconcile.sh against a fixture main and a fake
# GitHub. While main owes a stable release, it requests exactly one source
# candidate when none is in flight and main's newest commit has had none; it
# never loops on a source that already failed, alarms once that state outlasts
# the allowance, and treats every unread census as a failure.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

repo="$tmp/repo"
git init -q -b main "$repo"
git -C "$repo" config user.name "Reconcile Test"
git -C "$repo" config user.email "reconcile-test@example.com"
git -C "$repo" config commit.gpgsign false
commit_version() {
  printf '[workspace.package]\nversion = "%s"\n' "$1" > "$repo/Cargo.toml"
  printf '%s\n' "$2" > "$repo/stamp"
  git -C "$repo" add Cargo.toml stamp
  GIT_COMMITTER_DATE="@$2 +0000" GIT_AUTHOR_DATE="@$2 +0000" git -C "$repo" commit -q -m "$1 at $2"
  git -C "$repo" rev-parse HEAD
}
commit_version 1.2.3-dev 1000000 >/dev/null
release_commit="$(commit_version 1.2.3 2000000)"
main_sha="$(commit_version 1.2.3 2000100)"
dev_sha="$(git -C "$repo" rev-parse HEAD~2)"

mkdir -p "$tmp/bin"
# FAKE_TAGS: published tags (JSON array). FAKE_BUILDS, FAKE_PROMOTIONS and
# FAKE_ATTEMPTS: workflow_runs arrays for the in-flight build census, the
# promotion census and the attempts at main's commit. FAKE_CANDIDATE_SHAS:
# space-separated sources holding a successful candidate. FAKE_FAIL names one
# read (releases, builds, promotions, attempts) that cannot be read.
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
unread() { [[ "${FAKE_FAIL:-}" == "$1" ]] && exit 1; return 0; }
case "$1 $2" in
  "api repos/o/r/releases?per_page=50")
    unread releases
    jq -n --argjson tags "$FAKE_TAGS" '[$tags[] | {tag_name: ., draft: false}] + [{tag_name: "v9.9.9", draft: true}]' | jq -r "$4" ;;
  "api repos/o/r/actions/workflows/build-release-binaries.yml/runs?branch=main&status="*"&per_page=100")
    unread builds
    state="${2#*status=}"; state="${state%%&*}"
    if [[ "${FAKE_BAD_BUILD:-}" == 1 ]]; then
      jq -n '[{total_count: 1, workflow_runs: [{id: null, event: "workflow_dispatch", status: "in_progress", head_branch: "main", head_sha: "123"}]}]'
      exit 0
    fi
    pages="${FAKE_BUILD_PAGES:-}"
    [[ -n "$pages" ]] || pages="[${FAKE_BUILDS:-[]}]"
    jq -n --argjson pages "$pages" --arg state "$state" --arg override "${FAKE_BUILD_TOTAL:-}" '[ $pages[] | [.[] | select(.status == $state)] ] as $filtered | (if $override == "" then ($filtered | map(length) | add) else ($override | tonumber) end) as $total | [$filtered[] | {total_count: $total, workflow_runs: .}]' ;;
  "api repos/o/r/actions/workflows/promote-release.yml/runs?per_page=50")
    unread promotions
    jq -n --argjson runs "${FAKE_PROMOTIONS:-[]}" '{workflow_runs: $runs}' | jq -r "$4" ;;
  "api repos/o/r/actions/workflows/build-release-binaries.yml/runs?head_sha="*"&status=success&per_page=30")
    sha="${2#*head_sha=}"; sha="${sha%%&*}"
    if [[ " ${FAKE_CANDIDATE_SHAS:-} " == *" $sha "* ]]; then echo 77; fi ;;
  "api repos/o/r/actions/runs/77/artifacts?name=candidate-manifest-"*) echo 1 ;;
  "api repos/o/r/actions/runs/900/jobs?per_page=100")
    unread jobs
    jq -n --argjson jobs "${FAKE_JOBS:-[]}" --arg override "${FAKE_JOBS_TOTAL:-}" '[{total_count: (if $override == "" then ($jobs | length) else ($override | tonumber) end), jobs: $jobs}]' ;;
  "api repos/o/r/actions/workflows/build-release-binaries.yml/runs?head_sha="*"&per_page=100")
    unread attempts
    jq -n --argjson runs "${FAKE_ATTEMPTS:-[]}" '[{total_count: ($runs | length), workflow_runs: $runs}]' ;;
  "workflow run") ;;
  "issue list")
    jq -n --arg t "${FAKE_OPEN_ISSUE:-}" 'if $t == "" then [] else [{number: 7, title: $t}] end' | jq -r "${@: -1}" ;;
  "issue create") ;;
  *) echo "unexpected gh $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/gh"

run() {
  : > "$tmp/gh.log"
  set +e
  env PATH="$tmp/bin:$PATH" FAKE_GH_LOG="$tmp/gh.log" GITHUB_REPOSITORY=o/r FAKE_TAGS='["v1.2.2"]' \
    MAIN_SHA="$main_sha" SOURCE_DIRECTORY="$repo" SELF_RUN_ID=500 NOW_EPOCH="$young" "$@" \
    bash "$root/scripts/ci/release_candidate_reconcile.sh" > "$tmp/out" 2>&1
  status=$?
  set -e
}
dispatched() { grep -c '^workflow run build-release-binaries.yml --repo o/r --ref main -f source_candidate=true$' "$tmp/gh.log" || true; }
young=$((2000000 + 3600))
old=$((2000000 + 5 * 3600))
build() { jq -nc --arg event "$1" --arg sha "$2" --arg status "$3" --arg title "${4:-Build release binaries [source-candidate]}" '{id: 900, event: $event, head_sha: $sha, head_branch: "main", status: $status, display_title: $title}'; }

# Nothing owed: a development main, or a published version.
run MAIN_SHA="$dev_sha"
[[ $status == 0 && ! -s "$tmp/gh.log" ]] || fail "a development main was reconciled: $(cat "$tmp/out")"
run FAKE_TAGS='["v1.2.3"]'
[[ $status == 0 && "$(dispatched)" == 0 ]] || fail "a published version requested a candidate"

# Owed, nothing in flight, main's newest commit untried: request exactly one.
run
[[ $status == 0 && "$(dispatched)" == 1 ]] || fail "an idle owed version requested no candidate: $(cat "$tmp/out")"
grep -Fq "v1.2.3 owed since $release_commit" "$tmp/out" || fail "the request does not name the version commit"

# A candidate build in flight: any main source candidate, or the version push.
for inflight in "workflow_dispatch $main_sha in_progress" "schedule $dev_sha queued" "push $release_commit in_progress"; do
  read -r event sha state <<< "$inflight"
  run "FAKE_BUILDS=[$(build "$event" "$sha" "$state")]"
  [[ $status == 0 && "$(dispatched)" == 0 ]] || fail "a second candidate was requested beside $inflight"
  grep -Fq 'candidate builds in flight: [900]' "$tmp/out" || fail "the in-flight $event build was not reported"
done
# A warm push in flight, or a completed source candidate, is not a candidate in flight.
run "FAKE_BUILDS=[$(build push "$main_sha" in_progress),$(build workflow_dispatch "$dev_sha" completed)]"
[[ $status == 0 && "$(dispatched)" == 1 ]] || fail "a warm push or finished run blocked the request"
run "FAKE_BUILD_PAGES=[[],[$(build workflow_dispatch "$main_sha" in_progress)]]"
[[ $status == 0 && "$(dispatched)" == 0 ]] || fail "a candidate on a later page was missed: $(cat "$tmp/out")"
run FAKE_BUILD_TOTAL=1
[[ $status == 1 && "$(dispatched)" == 0 ]] || fail "an incomplete build census was treated as idle: $(cat "$tmp/out")"
run FAKE_BAD_BUILD=1
[[ $status == 1 && "$(dispatched)" == 0 ]] || fail "a build without an ID was treated as idle: $(cat "$tmp/out")"

# A benchmark-only dispatch must not count as an in-flight or attempted candidate.
benchmark="$(build workflow_dispatch "$main_sha" in_progress 'Build release binaries [benchmark]')"
run "FAKE_BUILDS=[$benchmark]"
[[ $status == 0 && "$(dispatched)" == 1 ]] || fail "a benchmark blocked an untried candidate: $(cat "$tmp/out")"
benchmark="$(build workflow_dispatch "$main_sha" completed 'Build release binaries [benchmark]')"
run "FAKE_ATTEMPTS=[$benchmark]" NOW_EPOCH="$old"
[[ $status == 0 && "$(dispatched)" == 1 ]] || fail "a completed benchmark counted as an attempted candidate: $(cat "$tmp/out")"

# Older dispatches expose their purpose through the setup job, or remain unknown.
legacy="$(build workflow_dispatch "$main_sha" in_progress 'Build release binaries')"
legacy_jobs='[{"name":"Resolve release context","steps":[{"name":"Run actions/checkout@v7","conclusion":"success"},{"name":"Install the source candidate decision runtime","conclusion":"skipped"}]}]'
run "FAKE_BUILDS=[$legacy]" "FAKE_JOBS=$legacy_jobs"
[[ $status == 0 && "$(dispatched)" == 1 ]] || fail "a legacy non-candidate blocked a request: $(cat "$tmp/out")"
legacy_attempt="$(build workflow_dispatch "$main_sha" completed 'Build release binaries')"
run "FAKE_ATTEMPTS=[$legacy_attempt]" "FAKE_JOBS=$legacy_jobs" NOW_EPOCH="$old"
[[ $status == 0 && "$(dispatched)" == 1 ]] || fail "a legacy benchmark counted as an attempt: $(cat "$tmp/out")"
legacy_source='[{"name":"Resolve release context","steps":[{"name":"Run actions/checkout@v7","conclusion":"success"},{"name":"Install the source candidate decision runtime","conclusion":"success"}]}]'
run "FAKE_BUILDS=[$legacy]" "FAKE_JOBS=$legacy_source"
[[ $status == 0 && "$(dispatched)" == 0 ]] || fail "a legacy source candidate did not block a duplicate: $(cat "$tmp/out")"
run "FAKE_ATTEMPTS=[$legacy_attempt]" "FAKE_JOBS=$legacy_source" NOW_EPOCH="$old"
[[ $status == 1 && "$(dispatched)" == 0 ]] || fail "a legacy source attempt was retried automatically: $(cat "$tmp/out")"
run "FAKE_BUILDS=[$legacy]"
[[ $status == 1 && "$(dispatched)" == 0 ]] || fail "unknown legacy dispatch was treated as idle or successful: $(cat "$tmp/out")"
run "FAKE_ATTEMPTS=[$legacy_attempt]" FAKE_FAIL=jobs
[[ $status == 1 && "$(dispatched)" == 0 ]] || fail "an unread legacy dispatch was treated as an idle attempt: $(cat "$tmp/out")"
run "FAKE_BUILDS=[$legacy]" "FAKE_JOBS=$legacy_jobs" FAKE_JOBS_TOTAL=2
[[ $status == 1 && "$(dispatched)" == 0 ]] || fail "an incomplete job census was treated as a non-candidate: $(cat "$tmp/out")"
failed_checkout='[{"name":"Resolve release context","steps":[{"name":"Run actions/checkout@v7","conclusion":"failure"},{"name":"Install the source candidate decision runtime","conclusion":"skipped"}]}]'
run "FAKE_BUILDS=[$legacy]" "FAKE_JOBS=$failed_checkout"
[[ $status == 1 && "$(dispatched)" == 0 ]] || fail "a skipped step after failed checkout was treated as a non-candidate: $(cat "$tmp/out")"

# A promotion in flight counts only when its source holds a candidate, and
# this run never counts itself.
promotion() { jq -nc --argjson id "$1" --arg sha "$2" '{id: $id, status: "in_progress", display_title: ("Promote release " + $sha)}'; }
run "FAKE_PROMOTIONS=[$(promotion 600 "$release_commit")]" "FAKE_CANDIDATE_SHAS=$release_commit"
[[ $status == 0 && "$(dispatched)" == 0 ]] || fail "a candidate was requested beside a candidate's promotion"
grep -Fq 'candidate promotions in flight: [600]' "$tmp/out" || fail "the in-flight promotion was not reported"
run "FAKE_PROMOTIONS=[$(promotion 600 "$main_sha"),$(promotion 500 "$release_commit")]" "FAKE_CANDIDATE_SHAS=$release_commit"
[[ $status == 0 && "$(dispatched)" == 1 ]] || fail "a candidate-less or self promotion blocked the request"

# One source candidate per main commit: a failed one never loops.
attempt="[$(build workflow_dispatch "$main_sha" completed)]"
run "FAKE_ATTEMPTS=$attempt"
[[ $status == 0 && "$(dispatched)" == 0 ]] || fail "a failed source was requested again: $(cat "$tmp/out")"
! grep -q '^issue create' "$tmp/gh.log" || fail "a young stranded version alarmed"
run "FAKE_ATTEMPTS=$attempt" NOW_EPOCH="$old"
[[ $status == 1 && "$(dispatched)" == 0 ]] || fail "an overdue failed source passed: $(cat "$tmp/out")"
grep -q "$release_commit (5h ago)" "$tmp/out" || fail "the alarm did not name the version commit"
grep -q '^issue create' "$tmp/gh.log" || fail "the overdue version opened no issue"
run "FAKE_ATTEMPTS=$attempt" NOW_EPOCH="$old" FAKE_OPEN_ISSUE="[Release] v1.2.3 is unpublished on main"
[[ $status == 1 ]] || fail "an overdue version with a tracking issue passed"
! grep -q '^issue create' "$tmp/gh.log" || fail "a second tracking issue was opened"
# In flight outranks overdue: the alarm waits for the measured outcome.
run "FAKE_ATTEMPTS=$attempt" NOW_EPOCH="$old" "FAKE_BUILDS=[$(build schedule "$main_sha" in_progress)]"
[[ $status == 0 ]] || fail "an in-flight candidate of an overdue version failed the run"

# Every unread census fails without requesting anything.
for read in releases builds promotions attempts; do
  run "FAKE_FAIL=$read"
  [[ $status == 1 && "$(dispatched)" == 0 ]] || fail "an unread $read census read as idle"
done

echo "release_candidate_reconcile_test: ok"
