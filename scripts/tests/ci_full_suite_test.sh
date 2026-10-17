#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *workflows/ci.yml/runs*) cat "$FIXTURE/runs" ;;
  *workflows/macos-nightly.yml/runs*) cat "$FIXTURE/native-runs" ;;
  *workflows/macos-nightly.yml*) cat "$FIXTURE/native-workflow" ;;
  *runs/81/attempts/1/jobs*) cat "$FIXTURE/native-jobs" ;;
  *runs/81/attempts/1*) cat "$FIXTURE/native-run" ;;
  *runs/82/attempts/1/jobs*) cat "$FIXTURE/newer-jobs" ;;
  *runs/82/attempts/1*) cat "$FIXTURE/newer-run" ;;
  *runs/71/jobs*) cat "$FIXTURE/jobs" ;;
  *) exit 9 ;;
esac
SH
chmod +x "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH" FIXTURE="$scratch" GH_REPO=fixture/repo
export SOURCE_SHA=1111111111111111111111111111111111111111
jq -nc '{id:64,path:".github/workflows/macos-nightly.yml",state:"active"}' > "$scratch/native-workflow"
jq -nc --arg source "$SOURCE_SHA" '{id:81,run_attempt:1,workflow_id:64,path:".github/workflows/macos-nightly.yml",
  head_sha:$source,event:"workflow_dispatch",status:"completed",conclusion:"success",
  repository:{full_name:"fixture/repo"},head_repository:{full_name:"fixture/repo"}}' > "$scratch/native-run"
jq -sc '[{total_count:1,workflow_runs:.}]' "$scratch/native-run" > "$scratch/native-runs"
jq -nc --arg source "$SOURCE_SHA" --slurpfile contract "$root/scripts/release_audit_contract.json" '
  [{total_count:1,jobs:[{id:91,run_id:81,run_attempt:1,head_sha:$source,name:"Workspace nextest (macos-latest)",
    status:"completed",conclusion:"success",steps:($contract[0].platforms[]|select(.name=="macos")|.required_steps|
      map({name:.,status:"completed",conclusion:"success"}))}]}]' > "$scratch/native-jobs"
mkdir "$scratch/good-native"
cp "$scratch"/native-* "$scratch/good-native/"
printf '71\t%s\tcompleted\tsuccess\n' "$SOURCE_SHA" > "$scratch/runs"
names=('CI status' 'Verify publishable crates' 'Stack frame budget' 'Rust workspace tests' 'Run Linux sandbox tests' 'Windows cross-compile check' 'Rust on macOS (deny-warnings build + lint)')
for name in "${names[@]}"; do printf '%s\tcompleted\tsuccess\n' "$name"; done > "$scratch/jobs"
bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log"
grep -q 'Full-suite CI succeeded' "$scratch/log"
grep -q '"status":"complete"' "$scratch/log"
for name in "${names[@]}"; do
  cp "$scratch/jobs" "$scratch/good"
  awk -F '\t' -v name="$name" '$1 != name' "$scratch/good" > "$scratch/jobs"
  if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
    echo "Missing proof accepted: $name" >&2; exit 1
  fi
  mv "$scratch/good" "$scratch/jobs"
done
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
native_refused missing-run
jq '.[0].total_count=2' "$scratch/good-native/native-runs" > "$scratch/native-runs"
native_refused partial-runs
jq '.[0].workflow_runs[0].head_sha="2222222222222222222222222222222222222222"' "$scratch/good-native/native-runs" > "$scratch/native-runs"
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
for index in 0 1 2; do
  jq --argjson index "$index" '.[0].jobs[0].steps[$index].conclusion="skipped"' "$scratch/good-native/native-jobs" > "$scratch/native-jobs"
  native_refused "skipped-step-$index"
done
jq '.[0].jobs += [.[0].jobs[0] | .id=92] | .[0].total_count=2' "$scratch/good-native/native-jobs" > "$scratch/native-jobs"
native_refused duplicate-required-job
jq '.[0].workflow_runs += [.[0].workflow_runs[0] | .id=82 | .conclusion="failure"] | .[0].total_count=2' "$scratch/good-native/native-runs" > "$scratch/native-runs"
jq '.id=82 | .conclusion="failure"' "$scratch/good-native/native-run" > "$scratch/newer-run"
jq '.[0].jobs[0].run_id=82' "$scratch/good-native/native-jobs" > "$scratch/newer-jobs"
native_refused newer-failure-after-prior-success
bash "$root/scripts/stage_release_tools.sh" "$scratch/staged"
mkdir "$scratch/other-checkout"
(cd "$scratch/other-checkout" && bash "$scratch/staged/require_full_suite.sh") > "$scratch/staged-log"
grep -q '"status":"complete"' "$scratch/staged-log"
echo 'Native publication: real phases required; missing, partial, foreign, failed, skipped, duplicate and superseded positives refuse; staged gate works from another checkout.'
