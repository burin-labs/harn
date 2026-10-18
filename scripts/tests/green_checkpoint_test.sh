#!/usr/bin/env bash
# Exercise scripts/ci/green_checkpoint.sh against a stubbed GitHub API.
#
# The stub keeps green, tags and tag objects as files, applies the script's own
# --jq programs, and refuses a non-fast-forward PATCH the way `force: false` does.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/green_checkpoint.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
C=cccccccccccccccccccccccccccccccccccccccc

# State in $S: green (sha or empty), relations ("base...head status" lines; a
# missing pair is unreadable), census.json, tags/<name> -> object, objects/<sha>.
cat > "$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
method=GET jq_program='' input='' endpoint='' fields=()
while (($#)); do
  case "$1" in
    api | --paginate | --slurp) shift ;;
    -X) method=$2; shift 2 ;;
    --jq) jq_program=$2; shift 2 ;;
    --input) input=$(cat); shift 2 ;;
    -f | -F) fields+=("$2"); shift 2 ;;
    *) endpoint=$1; shift ;;
  esac
done
respond() { if [[ -n "$jq_program" ]]; then jq -r "$jq_program"; else cat; fi; }
field() { local f; for f in "${fields[@]}"; do [[ "$f" != "$1="* ]] || { echo "${f#*=}"; return; }; done; }
relation() { awk -v spec="$1" '$1 == spec { print $2 }' "$S/relations"; }
case "$method $endpoint" in
  "GET repos/o/r/actions/workflows/ci.yml/runs?"*)
    [[ -z "${CENSUS_FAIL:-}" ]] || exit 1
    respond < "$S/census.json" ;;
  "GET repos/o/r/git/matching-refs/heads/green")
    [[ -z "${GREEN_FAIL:-}" ]] || exit 1
    jq -n --arg sha "$(cat "$S/green")" \
      'if $sha == "" then [] else [{ref: "refs/heads/green", object: {sha: $sha, type: "commit"}}] end' ;;
  "GET repos/o/r/compare/"*)
    spec=${endpoint#repos/o/r/compare/}
    status=$(relation "${spec%%\?*}")
    [[ -n "$status" ]] || exit 1
    jq -n --arg status "$status" '{status: $status}' | respond ;;
  "GET repos/o/r/git/matching-refs/tags/"*)
    name=${endpoint#repos/o/r/git/matching-refs/tags/}
    if [[ -f "$S/tags/$name" ]]; then
      jq -n --arg ref "refs/tags/$name" --arg sha "$(cat "$S/tags/$name")" '[{ref: $ref, object: {sha: $sha, type: "tag"}}]'
    else echo '[]'; fi ;;
  "GET repos/o/r/git/tags/"*) cat "$S/objects/${endpoint#repos/o/r/git/tags/}" ;;
  "POST repos/o/r/git/tags")
    object=$(git hash-object --stdin <<< "$input")
    jq '{object: {sha: .object, type: "commit"}, message: .message, tag: .tag}' <<< "$input" > "$S/objects/$object"
    echo "tag $(jq -r .tag <<< "$input")" >> "$S/writes"
    jq -n --arg sha "$object" '{sha: $sha}' | respond ;;
  "POST repos/o/r/git/refs")
    ref=$(field ref) sha=$(field sha)
    case "$ref" in
      refs/tags/*) printf '%s' "$sha" > "$S/tags/${ref#refs/tags/}" ;;
      refs/heads/green) [[ ! -s "$S/green" ]] || exit 1; printf '%s' "$sha" > "$S/green" ;;
      *) exit 99 ;;
    esac
    echo "create $ref $sha" >> "$S/writes"
    jq -n --arg sha "$sha" '{object: {sha: $sha}}' | respond ;;
  "PATCH repos/o/r/git/refs/heads/green")
    sha=$(field sha)
    [[ "$(field force)" == false && "$(relation "$(cat "$S/green")...$sha")" == ahead ]] || exit 1
    printf '%s' "$sha" > "$S/green"
    echo "advance $sha" >> "$S/writes"
    jq -n --arg sha "$sha" '{object: {sha: $sha}}' | respond ;;
  *) echo "unexpected gh call: $method $endpoint" >&2; exit 99 ;;
esac
EOF
chmod +x "$work/bin/gh"

# run <id> <sha> <created> -> one census row.
run() {
  jq -n --argjson id "$1" --arg sha "$2" --arg created "$3" '{id: $id, run_attempt: 1,
    head_sha: $sha, html_url: "https://github.com/o/r/actions/runs/\($id)",
    created_at: $created, updated_at: $created, path: ".github/workflows/ci.yml",
    event: "push", head_branch: "main", status: "completed", conclusion: "success",
    head_repository: {full_name: "o/r"}}'
}
census() { jq -s '[{total_count: length, workflow_runs: .}]'; }

failures=0
S=""
fresh() { # green [relation lines...]
  S="$work/case-$RANDOM$RANDOM"
  mkdir -p "$S/tags" "$S/objects"
  printf '%s' "$1" > "$S/green"
  shift
  printf '%s\n' "$@" > "$S/relations"
  : > "$S/writes"
}
check() { # name expected-exit expected-green expected-writes [env...]
  local name=$1 want_exit=$2 want_green=$3 want_writes=$4 status=0
  shift 4
  env PATH="$work/bin:$PATH" S="$S" GITHUB_REPOSITORY=o/r GITHUB_STEP_SUMMARY="$S/summary" \
    "$@" bash "$script" > "$S/out" 2>&1 || status=$?
  local got_writes
  got_writes=$(cut -d' ' -f1 "$S/writes" | paste -sd, -)
  if [[ "$status" == "$want_exit" && "$(cat "$S/green")" == "$want_green" && "$got_writes" == "$want_writes" ]]; then
    echo "ok   $name"
  else
    echo "FAIL $name: exit $status (want $want_exit), green $(cat "$S/green") (want $want_green), writes '$got_writes' (want '$want_writes')"
    sed 's/^/     /' "$S/out"
    failures=$((failures + 1))
  fi
}

# Older runs have no relation fixture: comparing one would fail the case, so
# these cases also prove the pass stops once green is settled.
fresh "" "$B...main ahead"
{ run 2 "$B" 2026-10-17T12:00:00Z; run 1 "$A" 2026-10-17T11:00:00Z; } | census > "$S/census.json"
check "first pass creates green at the newest certified commit" 0 "$B" tag,create,create
[[ -f "$S/tags/green-20261017T120000Z-2" ]] || { echo "FAIL tag name"; failures=$((failures + 1)); }

fresh "$A" "$A...$B ahead" "$B...main ahead"
{ run 1 "$A" 2026-10-17T11:00:00Z; run 2 "$B" 2026-10-17T12:00:00Z; } | census > "$S/census.json"
check "a newer certified commit advances green" 0 "$B" tag,create,advance
record=$(jq -r '.message | split("\n") | last' "$S/objects/$(cat "$S/tags/green-20261017T120000Z-2")")
if jq -e --arg sha "$B" '.schema == "harn.green_checkpoint.v1" and .sha == $sha and .runId == 2
    and .runAttempt == 1 and .runUrl == "https://github.com/o/r/actions/runs/2"
    and .completedAt == "2026-10-17T12:00:00Z"' <<< "$record" > /dev/null; then
  echo "ok   the tag message ends with the checkpoint record"
else
  echo "FAIL checkpoint record: $record"
  failures=$((failures + 1))
fi

fresh "$B" "$B...$B identical"
{ run 2 "$B" 2026-10-17T12:00:00Z; run 1 "$A" 2026-10-17T11:00:00Z; } | census > "$S/census.json"
check "certified commits green already contains write nothing" 0 "$B" ""

fresh "$A" "$A...$C ahead" "$C...main diverged"
run 3 "$C" 2026-10-17T13:00:00Z | census > "$S/census.json"
check "a run whose commit left main is skipped" 0 "$A" ""

fresh "$A" "$A...$B ahead" "$B...main ahead" "$A...main ahead" "$A...$A identical"
run 2 "$B" 2026-10-17T12:00:00Z | census > "$S/census.json"
env PATH="$work/bin:$PATH" S="$S" gh api -X POST repos/o/r/git/tags --input - > /dev/null <<< \
  "$(jq -n --arg sha "$B" '{tag: "green-20261017T120000Z-2", object: $sha,
    message: ("x\n\n" + ({schema: "harn.green_checkpoint.v1", sha: $sha, runId: 2} | tojson))}')"
printf '%s' "$(ls "$S/objects")" > "$S/tags/green-20261017T120000Z-2"
: > "$S/writes"
check "an existing tag for the same run is reused" 0 "$B" advance

fresh "$A" "$A...$C ahead" "$C...main ahead" "$A...$B ahead" "$B...main ahead"
{ run 3 "$C" 2026-10-17T13:00:00Z; run 2 "$B" 2026-10-17T12:00:00Z; } | census > "$S/census.json"
env PATH="$work/bin:$PATH" S="$S" gh api -X POST repos/o/r/git/tags --input - > /dev/null <<< \
  "$(jq -n --arg sha "$C" '{tag: "green-20261017T130000Z-3", object: $sha,
    message: ("x\n\n" + ({schema: "harn.green_checkpoint.v1", sha: $sha, runId: 99} | tojson))}')"
printf '%s' "$(ls "$S/objects")" > "$S/tags/green-20261017T130000Z-3"
: > "$S/writes"
check "a tag naming another run fails the pass but not the older run" 1 "$B" tag,create,advance

fresh "$A" "$A...$B ahead" "$B...main ahead"
{ run 3 "$C" 2026-10-17T13:00:00Z; run 2 "$B" 2026-10-17T12:00:00Z; } | census > "$S/census.json"
check "an unreadable comparison fails the pass after the older run advances" 1 "$B" tag,create,advance

fresh "$A" "$A...$B diverged" "$B...main ahead"
run 2 "$B" 2026-10-17T12:00:00Z | census > "$S/census.json"
check "a main commit diverged from green fails" 1 "$A" ""

fresh "$A" "$A...$B ahead" "$B...main ahead"
run 2 "$B" 2026-10-17T12:00:00Z | jq -s '[{total_count: 2, workflow_runs: .}]' > "$S/census.json"
check "an incomplete census writes nothing" 1 "$A" ""

fresh "$A" "$A...$B ahead" "$B...main ahead"
run 2 "$B" 2026-10-17T12:00:00Z | census > "$S/census.json"
check "an unreadable census writes nothing" 1 "$A" "" CENSUS_FAIL=1
check "an unreadable green writes nothing" 1 "$A" "" GREEN_FAIL=1

fresh "$A" "$A...$B ahead" "$B...main ahead"
run 2 abc 2026-10-17T12:00:00Z | census > "$S/census.json"
check "a malformed run writes nothing" 1 "$A" ""

fresh "$A"
census < /dev/null > "$S/census.json"
check "an empty census is a measured quiet main" 0 "$A" ""

if (( failures > 0 )); then
  echo "green checkpoint: $failures case(s) failed"
  exit 1
fi
echo "green checkpoint: all cases passed"
