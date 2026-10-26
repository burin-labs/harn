#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "${FAKE_GH_FAILURE:-false}" != true ]] || exit 1
case "$*" in
  'run watch '*)
    printf '%s\n' "$*" >> "$FIXTURE/watch-calls"
    case "${FAKE_WATCH_CASE:-}" in
      recover)
        if [[ $(wc -l < "$FIXTURE/watch-calls" | tr -d ' ') == 1 ]]; then
          echo 'failed to get run: HTTP 502: fixture interruption' >&2
          exit 1
        fi
        printf '71\t%s\tcompleted\tsuccess\n' "$SOURCE_SHA" > "$FIXTURE/runs" ;;
      refusal)
        echo 'failed to get run: HTTP 403: request detail HTTP 502: incidental' >&2
        exit 1 ;;
      exhausted)
        echo 'failed to get run: HTTP 503: fixture interruption' >&2
        exit 1 ;;
      failed)
        printf '71\t%s\tcompleted\tfailure\n' "$SOURCE_SHA" > "$FIXTURE/runs"
        exit 1 ;;
      *) exit 9 ;;
    esac ;;
  *workflows/ci.yml/runs*sort_by*) printf '71\n' ;;
  *workflows/ci.yml/runs*) cat "$FIXTURE/runs" ;;
  *workflows/macos-nightly.yml/runs*)
    if [[ ${FAKE_NATIVE_DISCOVERY:-false} == true && ! -f "$FIXTURE/discovery-read" ]]; then
      touch "$FIXTURE/discovery-read"
      printf '[{"total_count":0,"workflow_runs":[]}]\n'
    else
      cat "$FIXTURE/native-runs"
    fi ;;
  *workflows/macos-nightly.yml*) cat "$FIXTURE/native-workflow" ;;
  *runs/81/attempts/1/jobs*) cat "$FIXTURE/native-jobs" ;;
  *runs/81/attempts/1*) cat "$FIXTURE/native-run" ;;
  *runs/82/attempts/1/jobs*) cat "$FIXTURE/newer-jobs" ;;
  *runs/82/attempts/1*) cat "$FIXTURE/newer-run" ;;
  *runs/81/artifacts*) cat "$FIXTURE/native-artifacts" ;;
  *runs/82/artifacts*) cat "$FIXTURE/native-artifacts" ;;
  *artifacts/101/zip*) cat "$FIXTURE/native-receipt.zip" ;;
  *runs/71/jobs*) cat "$FIXTURE/jobs" ;;
  *) exit 9 ;;
esac
SH
chmod +x "$scratch/bin/gh"
cat > "$scratch/bin/sleep" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FIXTURE/discovery-waits"
SH
chmod +x "$scratch/bin/sleep"
export PATH="$scratch/bin:$PATH" FIXTURE="$scratch" GH_REPO=fixture/repo
export SOURCE_SHA=1111111111111111111111111111111111111111
export FULL_SUITE_STATUS_FILE="$scratch/source-ci.json"
jq -nc '{id:64,path:".github/workflows/macos-nightly.yml",state:"active"}' > "$scratch/native-workflow"
jq -nc --arg source "$SOURCE_SHA" '{id:81,run_attempt:1,workflow_id:64,path:".github/workflows/macos-nightly.yml",
  head_sha:"2222222222222222222222222222222222222222",head_branch:"main",display_title:("Native qualification " + $source),event:"workflow_dispatch",status:"completed",conclusion:"success",
  repository:{full_name:"fixture/repo"},head_repository:{full_name:"fixture/repo"}}' > "$scratch/native-run"
jq -sc '[{total_count:1,workflow_runs:.}]' "$scratch/native-run" > "$scratch/native-runs"
jq -nc --arg source "$SOURCE_SHA" --slurpfile contract "$root/scripts/release_audit_contract.json" '
  [{total_count:1,jobs:[{id:91,run_id:81,run_attempt:1,head_sha:$source,name:"Workspace nextest (macos-latest)",
    status:"completed",conclusion:"success",steps:($contract[0].platforms[]|select(.name=="macos")|.required_steps|
      map({name:.,status:"completed",conclusion:"success"}))}]}] | .[0].jobs[0].head_sha="2222222222222222222222222222222222222222"' > "$scratch/native-jobs"
jq -nc '[{total_count:1,artifacts:[{id:101,name:"native-qualification-1",expired:false,workflow_run:{id:81,head_sha:"2222222222222222222222222222222222222222"}}]}]' > "$scratch/native-artifacts"
jq -nc --arg source "$SOURCE_SHA" --slurpfile contract "$root/scripts/release_audit_contract.json" \
  '{kind:"harn.native_qualification.v1",repository:"fixture/repo",source_sha:$source,policy_sha:"2222222222222222222222222222222222222222",run_id:81,run_attempt:1,phases:($contract[0].platforms[]|select(.name=="macos")|.required_steps)}' > "$scratch/receipt.json"
(cd "$scratch" && zip -jq native-receipt.zip receipt.json)
cp "$scratch/receipt.json" "$scratch/good-receipt.json"
mkdir "$scratch/good-native"
cp "$scratch"/native-* "$scratch/good-native/"
printf '71\t%s\tcompleted\tsuccess\n' "$SOURCE_SHA" > "$scratch/runs"
names=('CI status' 'Verify publishable crates' 'Stack frame budget' 'Rust workspace tests' 'Run Linux sandbox tests' 'Windows cross-compile check' 'Rust on macOS (deny-warnings build + lint)')
for name in "${names[@]}"; do printf '%s\tcompleted\tsuccess\n' "$name"; done > "$scratch/jobs"
bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log"
grep -q 'Full-suite CI succeeded' "$scratch/log"
grep -q '"status":"complete"' "$scratch/log"
head -n 1 "$scratch/log" | jq -e \
  '.kind == "harn.release.full_suite.v1" and .verdict == "success" and .pending_count == 0 and .failed_count == 0 and .next_action == "continue"' >/dev/null
jq -e '.verdict == "success" and .next_action == "continue"' "$FULL_SUITE_STATUS_FILE" >/dev/null
printf '71\t%s\tin_progress\t\n' "$SOURCE_SHA" > "$scratch/runs"
: > "$scratch/watch-calls"
WAIT_FOR_FULL_SUITE=true FAKE_WATCH_CASE=recover \
  bash "$root/scripts/ci/require_full_suite.sh" --main-only > "$scratch/watch-recovery-log"
[[ $(wc -l < "$scratch/watch-calls" | tr -d ' ') == 2 ]]
[[ $(sort -u "$scratch/watch-calls") == 'run watch 71 --repo fixture/repo --exit-status' ]]
jq -e '.verdict == "success" and .pending_count == 0 and .failed_count == 0' "$FULL_SUITE_STATUS_FILE" >/dev/null
for scenario in refusal exhausted failed; do
  : > "$scratch/watch-calls"
  printf '71\t%s\tin_progress\t\n' "$SOURCE_SHA" > "$scratch/runs"
  if WAIT_FOR_FULL_SUITE=true FAKE_WATCH_CASE=$scenario \
    bash "$root/scripts/ci/require_full_suite.sh" --main-only > "$scratch/watch-$scenario-log"; then
    echo "Watch $scenario accepted as release proof" >&2; exit 1
  fi
  expected=1
  [[ $scenario != exhausted ]] || expected=3
  [[ $(wc -l < "$scratch/watch-calls" | tr -d ' ') == "$expected" ]]
  if [[ $scenario == failed ]]; then
    jq -e '.verdict == "failure"' "$FULL_SUITE_STATUS_FILE" >/dev/null
  else
    jq -e '.verdict == "unmeasured" and .reason == "watch" and .pending_count == null' "$FULL_SUITE_STATUS_FILE" >/dev/null
  fi
done
printf '71\t%s\tcompleted\tsuccess\n' "$SOURCE_SHA" > "$scratch/runs"
: > "$scratch/discovery-waits"
WAIT_FOR_FULL_SUITE=true FAKE_NATIVE_DISCOVERY=true NATIVE_QUALIFICATION_STATUS_FILE="$scratch/native-status.json" \
  bash "$root/scripts/ci/require_native_qualification.sh" "$GH_REPO" "$SOURCE_SHA" "$root/scripts/release_audit_contract.json" > "$scratch/discovery-log"
[[ $(cat "$scratch/discovery-waits") == 5 ]]
jq -e '.verdict == "success" and .source_sha == "1111111111111111111111111111111111111111" and .pending_count == 0 and .failed_count == 0' "$scratch/native-status.json" >/dev/null
: > "$scratch/discovery-waits"
jq '.[0].workflow_runs=[] | .[0].total_count=0' "$scratch/good-native/native-runs" > "$scratch/native-runs"
if WAIT_FOR_FULL_SUITE=true NATIVE_QUALIFICATION_STATUS_FILE="$scratch/native-status.json" \
  bash "$root/scripts/ci/require_native_qualification.sh" "$GH_REPO" "$SOURCE_SHA" "$root/scripts/release_audit_contract.json" > "$scratch/missing-discovery-log"; then
  echo 'Unreported native qualification passed admission' >&2; exit 1
fi
[[ $(wc -l < "$scratch/discovery-waits" | tr -d ' ') == 24 ]]
jq -e '.verdict == "failure" and .pending_count == 1 and .failed_count == 0 and .platforms[0].status == "missing"' "$scratch/native-status.json" >/dev/null
cp "$scratch/good-native/native-runs" "$scratch/native-runs"
jq '.conclusion="failure"' "$scratch/good-native/native-run" > "$scratch/native-run"
jq '.[0].artifacts=[] | .[0].total_count=0' "$scratch/good-native/native-artifacts" > "$scratch/native-artifacts"
if bash "$root/scripts/ci/require_native_qualification.sh" "$GH_REPO" "$SOURCE_SHA" "$root/scripts/release_audit_contract.json" > "$scratch/failed-native-log"; then
  echo 'A failed native execution passed without a receipt' >&2; exit 1
fi
jq -e '.verdict == "failure" and .failed_count == 1 and .failing_jobs == ["workflow run"]' "$scratch/failed-native-log" >/dev/null
cp "$scratch/good-native/"* "$scratch/"
for name in "${names[@]}"; do
  cp "$scratch/jobs" "$scratch/good"
  awk -F '\t' -v name="$name" '$1 != name' "$scratch/good" > "$scratch/jobs"
  if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
    echo "Missing proof accepted: $name" >&2; exit 1
  fi
  if bash "$root/scripts/ci/require_full_suite.sh" --main-only > "$scratch/main-only-log" 2>&1; then
    echo "Missing source proof accepted before building: $name" >&2; exit 1
  fi
  head -n 1 "$scratch/main-only-log" | jq -e --arg name "$name" \
    '.verdict == "failure" and .pending_count == 1 and .missing_jobs == [$name] and .next_action == "repair-source"' >/dev/null
  mv "$scratch/good" "$scratch/jobs"
done
cp "$scratch/jobs" "$scratch/good-jobs"
printf 'CI status\tcompleted\tsuccess\n' >> "$scratch/jobs"
if bash "$root/scripts/ci/require_full_suite.sh" --main-only > "$scratch/log" 2>&1; then
  echo 'Duplicate required job accepted as source proof' >&2; exit 1
fi
cp "$scratch/good-jobs" "$scratch/jobs"
awk -F '\t' 'BEGIN { OFS="\t" } $1 == "CI status" { $3="failure" } { print }' "$scratch/good-jobs" > "$scratch/jobs"
printf '71\t%s\tcompleted\tfailure\n' "$SOURCE_SHA" > "$scratch/runs"
if bash "$root/scripts/ci/require_full_suite.sh" --main-only > "$scratch/log" 2>&1; then
  echo 'Failed source admitted before building' >&2; exit 1
fi
head -n 1 "$scratch/log" | jq -e '.verdict == "failure" and .failing_jobs == ["CI status"] and .failed_count == 1' >/dev/null
cp "$scratch/good-jobs" "$scratch/jobs"
if FAKE_GH_FAILURE=true bash "$root/scripts/ci/require_full_suite.sh" --main-only > "$scratch/log" 2>&1; then
  echo 'Unreadable CI admitted before building' >&2; exit 1
fi
jq -e '.verdict == "unmeasured" and .pending_count == null and .next_action == "retry-read"' "$scratch/log" >/dev/null
jq -e '.verdict == "unmeasured" and .pending_count == null' "$FULL_SUITE_STATUS_FILE" >/dev/null
for outcome in failure cancelled ''; do
  printf '71\t%s\tcompleted\t%s\n' "$SOURCE_SHA" "$outcome" > "$scratch/runs"
  if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
    echo "Unqualified run accepted: $outcome" >&2; exit 1
  fi
done
: > "$scratch/runs"
if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
  echo 'No run was accepted as proof' >&2; exit 1
fi
printf '71\t%s\tcompleted\tsuccess\n' 2222222222222222222222222222222222222222 > "$scratch/runs"
if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
  echo 'Another commit was accepted as proof' >&2; exit 1
fi
echo 'Full-suite release gate: complete proof passes; missing families, missing runs, failures, and wrong source fail.'

# Restore a known nonempty main-CI positive before isolating native refusals.
printf '71\t%s\tcompleted\tsuccess\n' "$SOURCE_SHA" > "$scratch/runs"
native_refused() {
  if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
    echo "Unqualified native proof accepted: $1" >&2
    exit 1
  fi
  cp "$scratch/good-native/"* "$scratch/"
}
jq '.[0].workflow_runs=[] | .[0].total_count=0' "$scratch/good-native/native-runs" > "$scratch/native-runs"
# Admission proves main CI without waiting for the native run it will start.
# The same source still cannot publish without native qualification.
bash "$root/scripts/ci/require_full_suite.sh" --main-only > "$scratch/main-only-log"
grep -q 'Full-suite CI succeeded' "$scratch/main-only-log"
native_refused missing-run
jq '.[0].total_count=2' "$scratch/good-native/native-runs" > "$scratch/native-runs"
native_refused partial-runs
jq '.[0].workflow_runs[0].head_sha="3333333333333333333333333333333333333333"' "$scratch/good-native/native-runs" > "$scratch/native-runs"
native_refused foreign-run
jq '.conclusion="failure"' "$scratch/good-native/native-run" > "$scratch/native-run"
native_refused failed-run
jq '.status="in_progress" | .conclusion=null' "$scratch/good-native/native-run" > "$scratch/native-run"
native_refused pending-run
jq '.[0].total_count=2' "$scratch/good-native/native-jobs" > "$scratch/native-jobs"
native_refused partial-jobs
jq '.[0].jobs=[] | .[0].total_count=0' "$scratch/good-native/native-jobs" > "$scratch/native-jobs"
native_refused unreported-jobs
jq '.[0].jobs[0].run_attempt=2' "$scratch/good-native/native-jobs" > "$scratch/native-jobs"
native_refused foreign-attempt
jq '.[0].jobs[0].steps=[]' "$scratch/good-native/native-jobs" > "$scratch/native-jobs"
native_refused no-executed-steps
for index in $(jq '.platforms[]|select(.name=="macos")|.required_steps|keys[]' "$root/scripts/release_audit_contract.json"); do
  jq --argjson index "$index" '.[0].jobs[0].steps[$index].conclusion="skipped"' "$scratch/good-native/native-jobs" > "$scratch/native-jobs"
  native_refused "skipped-step-$index"
done
jq '.[0].jobs += [.[0].jobs[0] | .id=92] | .[0].total_count=2' "$scratch/good-native/native-jobs" > "$scratch/native-jobs"
native_refused duplicate-required-job
jq '.[0].workflow_runs += [.[0].workflow_runs[0] | .id=82 | .conclusion="failure"] | .[0].total_count=2' "$scratch/good-native/native-runs" > "$scratch/native-runs"
jq '.id=82 | .conclusion="failure"' "$scratch/good-native/native-run" > "$scratch/newer-run"
jq '.[0].jobs[0].run_id=82' "$scratch/good-native/native-jobs" > "$scratch/newer-jobs"
native_refused newer-failure-after-prior-success
for defect in '.source_sha="3333333333333333333333333333333333333333"' '.policy_sha="3333333333333333333333333333333333333333"' '.run_id=82' '.run_attempt=2' '.phases=[]' '.extra=true'; do
  jq "$defect" "$scratch/good-receipt.json" > "$scratch/receipt.json"
  (cd "$scratch" && zip -jq native-receipt.zip receipt.json)
  native_refused "receipt-$defect"
done
jq '.[0].artifacts=[]|.[0].total_count=0' "$scratch/good-native/native-artifacts" > "$scratch/native-artifacts"
native_refused missing-receipt
bash "$root/scripts/stage_release_tools.sh" "$scratch/staged"
mkdir "$scratch/other-checkout"
(cd "$scratch/other-checkout" && bash "$scratch/staged/require_full_suite.sh") > "$scratch/staged-log"
grep -q '"status":"complete"' "$scratch/staged-log"
echo 'Native publication: real phases required; missing, partial, foreign, failed, skipped, duplicate and superseded positives refuse; staged gate works from another checkout.'
