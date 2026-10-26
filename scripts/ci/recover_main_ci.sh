#!/usr/bin/env bash
set -euo pipefail
: "${GH_REPO:?repository required}"
: "${RUN_ID:?run required}"
[[ "$RUN_ID" =~ ^[0-9]+$ ]] || exit 1
read_api_json() {
  local endpoint="$1" mode="${2:-single}" response status attempt diagnostic
  local flags=()
  case "$mode" in
    single) ;;
    paginated) flags=(--paginate --slurp) ;;
    *) return 1 ;;
  esac
  for attempt in 1 2 3; do
    diagnostic=$(mktemp) || return 1
    if response=$(gh api ${flags[@]+"${flags[@]}"} "$endpoint" 2> "$diagnostic"); then
      cat "$diagnostic" >&2
      rm -f "$diagnostic"
      printf '%s\n' "$response"
      return 0
    fi
    cat "$diagnostic" >&2
    status=$(sed -nE 's/^gh:.* \(HTTP ([0-9]{3})\)$/\1/p' "$diagnostic")
    rm -f "$diagnostic"
    # Discard the entire failed attempt, including any successful earlier pages.
    if [[ -z "$status" ]]; then
      status=$(jq -er '
        (if type == "array" then .[-1] else . end)
        | select(type == "object") | .status | tonumber
      ' <<< "$response" 2>/dev/null) || status=unreported
    fi
    case "$status" in
      500|502|503|504) ;;
      *) echo "GitHub metadata read refused; no retry: $endpoint" >&2; return 1 ;;
    esac
    echo "GitHub metadata read HTTP $status, attempt $attempt/3: $endpoint" >&2
    (( attempt < 3 )) || return 1
    sleep "$attempt"
  done
}
run=$(read_api_json "repos/$GH_REPO/actions/runs/$RUN_ID")
jq -e --arg repo "$GH_REPO" '.event == "push" and .head_branch == "main" and .head_repository.full_name == $repo and .name == "CI" and .status == "completed"' <<< "$run" >/dev/null
sha=$(jq -er '.head_sha' <<< "$run")
attempt=$(jq -er '.run_attempt' <<< "$run")
conclusion=$(jq -er '.conclusion' <<< "$run")
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || exit 1
[[ "$attempt" =~ ^[1-9][0-9]*$ ]] || exit 1
if [[ "$conclusion" != failure && "$conclusion" != success ]]; then
  echo "No automatic action for $conclusion."
  exit 0
fi
if [[ "$conclusion" == failure && "$attempt" == 1 ]]; then
  gh run rerun "$RUN_ID" --repo "$GH_REPO" --failed
  exit 0
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
run_url="https://github.com/$GH_REPO/actions/runs/$RUN_ID"
# One failure cause owns one issue. The cause is a failing job's signature: its
# name, failed steps, and the normalized failure lines from its log. Open
# signature issues are read from the issue listing, not search, so a repeat
# seconds after the first occurrence still finds its issue.
read_api_json "repos/$GH_REPO/issues?state=open&per_page=100" paginated > "$scratch/issue-pages.json"
jq '
  def positive_integer: type == "number" and . > 0 and floor == .;
  def issue_valid:
    type == "object" and (.number | positive_integer)
    and (.body == null or (.body | type == "string"))
    and (.pull_request == null or (.pull_request | type == "object"));
  if type != "array" or length == 0 then error("missing issue pages") else . end
  | if all(.[]; type == "array" and all(.[]; issue_valid))
    then [.[][] | select(.pull_request == null and ((.body // "") | contains("<!-- main-ci-signature:")))]
    else error("malformed issue census") end
' "$scratch/issue-pages.json" > "$scratch/open.json"

# Close each open signature issue whose last occurrence was persistent once its
# job passes on this run's source or a descendant of the source it failed on.
# Issues whose last occurrence was a flake stay open: one green run proves
# nothing about a flake.
close_recovered() {
  local touched="$1" passed="$2" number body job last_sha status
  while IFS= read -r issue <&3; do
    number=$(jq -r '.number' <<< "$issue")
    body=$(jq -r '.body | gsub("\r"; "")' <<< "$issue")
    grep -Fxq "$(grep -oE 'main-ci-signature:[0-9a-f]+' <<< "$body" | head -1)" "$touched" && continue
    grep -Eq '^<!-- main-ci-last:persistent [0-9]+ [0-9a-f]{40} -->$' <<< "$body" || continue
    job=$(sed -nE 's/^<!-- main-ci-job:(.*) -->$/\1/p' <<< "$body" | head -1)
    [[ -n "$job" ]] || continue
    if [[ "$passed" != all ]] && ! grep -Fxq -- "$job" "$passed"; then continue; fi
    last_sha=$(sed -nE 's/^<!-- main-ci-last:persistent [0-9]+ ([0-9a-f]{40}) -->$/\1/p' <<< "$body")
    status=$(gh api "repos/$GH_REPO/compare/$last_sha...$sha" --jq '.status' 2>/dev/null || true)
    [[ "$status" == ahead || "$status" == identical ]] || continue
    gh issue close "$number" --repo "$GH_REPO" --comment "\`$job\` passed on main at \`$sha\`: $run_url"
  done 3< <(jq -c '.[]' "$scratch/open.json")
}

if [[ "$conclusion" == success && "$attempt" == 1 ]]; then
  : > "$scratch/touched"
  close_recovered "$scratch/touched" all
  exit 0
fi

cat > "$scratch/census.jq" <<'JQ'
def positive_integer: type == "number" and . > 0 and floor == .;
def named: .name | type == "string" and length > 0;
def status_valid: .status | IN("queued", "in_progress", "completed", "waiting", "pending", "requested");
def conclusion_valid: .conclusion == null or (.conclusion | type == "string");
def step_valid: type == "object" and named and status_valid and conclusion_valid;
def job_valid:
  type == "object" and (.id | positive_integer) and named and status_valid and conclusion_valid
  and (.steps == null or (.steps | type == "array" and all(.[]; step_valid)));
if type != "array" or length == 0 then error("missing job pages") else . end
| . as $pages
| if all($pages[]; type == "object" and (.jobs | type == "array") and (.total_count | positive_integer))
  then [$pages[] | .jobs[]] else error("malformed job census") end
| . as $jobs
| if ($pages[0].total_count != length)
     or any($pages[]; .total_count != $pages[0].total_count)
     or (all($jobs[]; job_valid) | not)
     or ([$jobs[].id] | unique | length) != length
  then error("incomplete or malformed job census") else . end
JQ
read_api_json "repos/$GH_REPO/actions/runs/$RUN_ID/attempts/1/jobs?per_page=100" paginated > "$scratch/first-pages.json"
jq -f "$scratch/census.jq" "$scratch/first-pages.json" > "$scratch/first.json"
read_api_json "repos/$GH_REPO/actions/runs/$RUN_ID/jobs?filter=latest&per_page=100" paginated > "$scratch/latest-pages.json"
jq -f "$scratch/census.jq" "$scratch/latest-pages.json" > "$scratch/latest.json"
# A job failing on the latest attempt is persistent; one that failed only on the
# first attempt is a flake. Each record names the attempt whose log shows it.
jq -n --slurpfile first "$scratch/first.json" --slurpfile latest "$scratch/latest.json" '
  def failing: [.[] | select(.conclusion == "failure" and .name != "CI status")];
  ($latest[0] | failing) as $persistent
  | ($persistent | map(.name)) as $names
  | [($persistent[] | {kind: "persistent", job: .}),
     ($first[0] | failing[] | select(.name as $n | $names | index($n) | not) | {kind: "flaky", job: .})]
  | .[] | {kind, id: .job.id, name: .job.name,
           steps: [.job.steps[]? | select(.conclusion == "failure") | .name]}' -c > "$scratch/failures.jsonl"
[[ -s "$scratch/failures.jsonl" ]] || { echo 'No measured failures; refusing classification.' >&2; exit 1; }
if [[ "$conclusion" == failure ]] && ! grep -q '"kind":"persistent"' "$scratch/failures.jsonl"; then
  echo 'Retry is red without a measured failing job; refusing classification.' >&2
  exit 1
fi
jq -r '.[] | select(.conclusion == "success") | .name' "$scratch/latest.json" > "$scratch/passed"
: > "$scratch/touched"

esc=$'\033'
# Job logs carry color codes, which newer gh refuses to print without this flag.
log_flags=()
gh_api_help=$(gh api --help 2>/dev/null || true)
[[ "$gh_api_help" != *--allow-escape-sequences* ]] || log_flags=(--allow-escape-sequences)
while IFS= read -r failure <&3; do
  kind=$(jq -r '.kind' <<< "$failure")
  job_id=$(jq -r '.id' <<< "$failure")
  job=$(jq -r '.name' <<< "$failure")
  # Failure lines Harn's gates print. Timestamps, colors, and numbers vary by
  # run, so they are normalized out of the signature but kept for display. An
  # unreadable log still files under the job and step signature.
  log_state=readable
  gh api ${log_flags[@]+"${log_flags[@]}"} "repos/$GH_REPO/actions/jobs/$job_id/logs" > "$scratch/job.log" 2>/dev/null || { log_state=unavailable; : > "$scratch/job.log"; }
  sed -E "s/${esc}\[[0-9;]*m//g; s/^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z //" "$scratch/job.log" \
    | grep -E '^(FAIL(  |: )|##\[error\]|make(\[[0-9]+\])?: \*\*\* )' \
    | grep -v '^##\[error\]Process completed with exit code' \
    | sed -E 's/^make(\[[0-9]+\])?: \*\*\* \[[^]]*:[0-9]+: ([^]]+)\].*/make target \2/' \
    | awk '!seen[$0]++' > "$scratch/lines" || true
  {
    printf 'job: %s\n' "$job"
    jq -r '.steps[] | "step: " + .' <<< "$failure"
    sed -E 's/ \([0-9.]+m?s\)$//; s/[0-9]+/N/g' "$scratch/lines" | sort -u
  } > "$scratch/signature"
  hash=$(git hash-object --stdin < "$scratch/signature" | cut -c1-16)
  echo "main-ci-signature:$hash" >> "$scratch/touched"
  summary=$(head -1 "$scratch/lines")
  [[ -n "$summary" ]] || summary=$(jq -r '.steps[0] // "failed"' <<< "$failure")
  last="<!-- main-ci-last:$kind $RUN_ID $sha -->"
  {
    # shellcheck disable=SC2016 # Markdown code spans, not shell substitutions.
    printf '%s occurrence at `%s`: %s\nJob log: %s\n' "$kind" "${sha:0:12}" "$run_url" "$log_state"
    if [[ -s "$scratch/lines" ]]; then
      printf '\n```text\n'; head -40 "$scratch/lines"; printf '```\n'
    fi
  } > "$scratch/occurrence"

  existing=$(jq -r --arg m "<!-- main-ci-signature:$hash -->" '[.[] | select(.body | contains($m))][0].number // empty' "$scratch/open.json")
  if [[ -z "$existing" ]]; then
    existing=$(gh issue list --repo "$GH_REPO" --state closed --search "\"main-ci-signature:$hash\" in:body" --json number --jq '.[0].number // empty')
    [[ -z "$existing" ]] || gh issue reopen "$existing" --repo "$GH_REPO"
  fi
  if [[ -n "$existing" ]]; then
    body=$(gh issue view "$existing" --repo "$GH_REPO" --json body --jq '.body | gsub("\r"; "")')
    if grep -Eq "^<!-- main-ci-last:[a-z]+ $RUN_ID " <<< "$body"; then
      echo "Run already recorded in issue #$existing."
      continue
    fi
    sed -E "s|^<!-- main-ci-last:.* -->$|$last|" <<< "$body" > "$scratch/body"
    gh issue edit "$existing" --repo "$GH_REPO" --body-file "$scratch/body"
    gh issue comment "$existing" --repo "$GH_REPO" --body-file "$scratch/occurrence"
    continue
  fi
  {
    printf '<!-- main-ci-signature:%s -->\n<!-- main-ci-job:%s -->\n%s\n\n' "$hash" "$job" "$last"
    printf 'Main CI fails with this signature. Repeats are comments here; a persistent failure closes when its job passes on main. Source causality is unestablished; submit any source correction through an independently reviewed PR.\n\n'
    printf 'Signature:\n```text\n'; cat "$scratch/signature"; printf '```\n\n'
    cat "$scratch/occurrence"
  } > "$scratch/body"
  gh issue create --repo "$GH_REPO" --title "[CI] Main fails in $job: ${summary:0:100}" --body-file "$scratch/body"
done 3< "$scratch/failures.jsonl"

close_recovered "$scratch/touched" "$scratch/passed"
