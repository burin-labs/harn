#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'api --paginate --slurp repos/fixture/repo/actions/runs/71/attempts/2/jobs?per_page=100' ]] || exit 9
printf 'read\n' >> "$FIXTURE/reads"
if [[ ${GH_FIXTURE_MODE:-} == materializing ]]; then
  case $(( $(wc -l < "$FIXTURE/reads") )) in
    1) printf '[{"total_count":0,"jobs":[]}]\n'; exit 0 ;;
    2) jq '[{total_count:2,jobs:.[0].jobs}]' "$FIXTURE/complete.json"; exit 0 ;;
  esac
fi
if [[ ${GH_FIXTURE_MODE:-} == failed-then-green ]] && (( $(wc -l < "$FIXTURE/reads") == 1 )); then
  jq '.[1].jobs[0].conclusion="failure"' "$FIXTURE/complete.json"
  exit 0
fi
if [[ ${GH_FIXTURE_MODE:-} == gateway ]] && (( $(wc -l < "$FIXTURE/reads") < 3 )); then
  printf '[{"total_count":0,"jobs":[]}]\n'
  echo 'gh: Server Error (HTTP 502)' >&2
  exit 23
fi
if [[ ${GH_FIXTURE_MODE:-} == unavailable || ${GH_FIXTURE_MODE:-} == denied ]]; then
  printf '[{"total_count":0,"jobs":[]}]\n'
  if [[ "$GH_FIXTURE_MODE" == unavailable ]]; then
    echo 'gh: Server Error (HTTP 502)' >&2
  else
    echo 'gh: Forbidden (HTTP 403)' >&2
  fi
  exit 23
fi
[[ ${GH_FIXTURE_FAIL:-0} == 0 ]] || exit 23
cat "$FIXTURE/jobs.json"
SH
cat > "$scratch/bin/sleep" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$1" >> "$FIXTURE/delays"
SH
chmod +x "$scratch/bin/gh" "$scratch/bin/sleep"
export PATH="$scratch/bin:$PATH" FIXTURE="$scratch" GH_REPO=fixture/repo
export GITHUB_RUN_ID=71 GITHUB_RUN_ATTEMPT=2 MATRIX_RESULT=success
export GITHUB_SHA=1111111111111111111111111111111111111111
legs=$(bash "$root/scripts/ci/run_rust_lint_lane.sh" --list-legs)
printf '%s\n' "$legs" | jq -Rsc '
  split("\n") | map(select(length > 0))
  | to_entries | map({id:(.key+1),name:("Rust on macOS strict lint (" + .value + ")"),status:"completed",conclusion:"success",run_id:71,run_attempt:2,head_sha:env.GITHUB_SHA})
  | [{total_count:3,jobs:.[0:2]},{total_count:3,jobs:.[2:3]}]
' > "$scratch/complete.json"
cp "$scratch/complete.json" "$scratch/jobs.json"
bash "$root/scripts/ci/require_macos_lint.sh" > "$scratch/log"
grep -q 'expected=3 observed=3 pending=0 bad=0' "$scratch/log"
echo 'PASS: complete paginated non-null current-attempt proof succeeds'

refuse() {
  if bash "$root/scripts/ci/require_macos_lint.sh" > "$scratch/log" 2>&1; then
    echo "Unqualified macOS proof accepted: $1" >&2
    exit 1
  fi
}
for conclusion in failure cancelled skipped neutral; do
  jq --arg conclusion "$conclusion" '.[1].jobs[0].conclusion=$conclusion' "$scratch/complete.json" > "$scratch/jobs.json"
  refuse "$conclusion"
  grep -q 'observed=3 pending=0 bad=1' "$scratch/log"
done
jq '.[1].jobs[0].status="in_progress" | .[1].jobs[0].conclusion=null' "$scratch/complete.json" > "$scratch/jobs.json"
refuse pending
grep -q 'observed=3 pending=1 bad=0' "$scratch/log"
jq '[{total_count:2,jobs:.[0].jobs}]' "$scratch/complete.json" > "$scratch/jobs.json"
refuse missing
grep -q 'observed=2 pending=1 bad=0' "$scratch/log"
rm -f "$scratch/reads" "$scratch/delays"
jq '.[1].jobs[0].name=.[0].jobs[0].name' "$scratch/complete.json" > "$scratch/jobs.json"
refuse duplicate
(( $(wc -l < "$scratch/reads") == 1 ))
test ! -e "$scratch/delays"
for census in '[]' '[{total_count:0,jobs:[]}]' '[{total_count:3,jobs:[]}]' '[{total_count:3,jobs:[]},{total_count:2,jobs:[]}]'; do
  printf '%s\n' "$census" > "$scratch/jobs.json"
  refuse empty-or-partial
done
cp "$scratch/complete.json" "$scratch/jobs.json"
MATRIX_RESULT=failure refuse matrix-failure
MATRIX_RESULT=skipped refuse matrix-skipped
GH_FIXTURE_FAIL=1 refuse unreadable-api
echo 'PASS: failed, skipped, pending, missing, duplicate, empty, partial and inconsistent proof refuses'

rm -f "$scratch/reads" "$scratch/delays"
GH_FIXTURE_MODE=gateway bash "$root/scripts/ci/require_macos_lint.sh" > "$scratch/log" 2>&1
grep -q 'expected=3 observed=3 pending=0 bad=0' "$scratch/log"
(( $(wc -l < "$scratch/reads") == 3 ))
[[ $(cat "$scratch/delays") == $'1\n2' ]]
echo 'PASS: transient gateways retry the whole paginated census and discard failed output'

rm -f "$scratch/reads" "$scratch/delays"
GH_FIXTURE_MODE=unavailable refuse persistent-gateway
(( $(wc -l < "$scratch/reads") == 3 ))
[[ $(cat "$scratch/delays") == $'1\n2' ]]
rm -f "$scratch/reads" "$scratch/delays"
GH_FIXTURE_MODE=denied refuse denied-api
(( $(wc -l < "$scratch/reads") == 1 ))
test ! -e "$scratch/delays"
echo 'PASS: exhausted gateways and permission failures refuse without accepting failed output'

rm -f "$scratch/reads" "$scratch/delays"
GH_FIXTURE_MODE=materializing bash "$root/scripts/ci/require_macos_lint.sh" > "$scratch/log" 2>&1
grep -q 'expected=3 observed=3 pending=0 bad=0' "$scratch/log"
(( $(wc -l < "$scratch/reads") == 3 ))
[[ $(cat "$scratch/delays") == $'30\n30' ]]
echo 'PASS: delayed inherited jobs require a fresh complete exact-attempt snapshot'

rm -f "$scratch/reads" "$scratch/delays"
GH_FIXTURE_MODE=failed-then-green refuse measured-failure
(( $(wc -l < "$scratch/reads") == 1 ))
test ! -e "$scratch/delays"
echo 'PASS: a measured failed leg refuses immediately even if later metadata would be green'

rm -f "$scratch/reads" "$scratch/delays"
printf '[{"total_count":0,"jobs":[]}]\n' > "$scratch/jobs.json"
refuse permanently-missing
(( $(wc -l < "$scratch/reads") == 7 ))
(( $(wc -l < "$scratch/delays") == 6 ))
[[ $(sort -u "$scratch/delays") == 30 ]]
echo 'PASS: permanently absent jobs exhaust the bounded wait and refuse'

for identity in '.run_id=72' '.run_attempt=1' '.head_sha="2222222222222222222222222222222222222222"' '.id=0' 'del(.id)' '.id=2'; do
  rm -f "$scratch/reads" "$scratch/delays"
  jq ".[0].jobs[0] |= ($identity)" "$scratch/complete.json" > "$scratch/jobs.json"
  refuse foreign-job
  (( $(wc -l < "$scratch/reads") == 1 ))
  test ! -e "$scratch/delays"
done
echo 'PASS: foreign run, attempt, source and invalid physical job identities refuse before reconciliation'
echo 'macos_lint_aggregate_test: ok'
