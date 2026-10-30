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
legs=$(bash "$root/scripts/ci/run_rust_lint_lane.sh" --list-legs)
printf '%s\n' "$legs" | jq -Rsc '
  split("\n") | map(select(length > 0))
  | map({name:("Rust on macOS strict lint (" + . + ")"),status:"completed",conclusion:"success"})
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
jq '.[1].jobs[0]=.[0].jobs[0]' "$scratch/complete.json" > "$scratch/jobs.json"
refuse duplicate
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
echo 'macos_lint_aggregate_test: ok'
