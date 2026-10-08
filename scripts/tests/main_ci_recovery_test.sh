#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/repo/scripts/ci"
cp "$root/scripts/ci/require_full_suite.sh" "$scratch/repo/scripts/ci/"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FIXTURE/calls"
case "$*" in
  'api repos/fixture/repo/actions/runs/42')
    jq -n --arg sha "$FAILED_SHA" --arg conclusion "$OUTCOME" --arg event "${EVENT:-push}" --argjson attempt "$ATTEMPT" \
      '{name: "CI", event: $event, head_branch: "main", head_repository: {full_name: "fixture/repo"}, status: "completed", head_sha: $sha, conclusion: $conclusion, run_attempt: $attempt}'
    ;;
  *attempts/1/jobs*) [[ "${NO_JOBS:-false}" == true ]] || echo 'Rust workspace tests' ;;
  *runs/42/jobs*) echo "${RETRY_JOB:-Rust workspace tests}" ;;
  *workflows/ci.yml/runs*)
    if [[ "${PARENT_RED:-false}" != true ]]; then
      printf '71\t%s\tcompleted\tsuccess\n' "$SOURCE_SHA"
    fi
    ;;
  *runs/71/jobs*)
    for name in 'CI status' 'Verify publishable crates' 'Stack frame budget' 'Rust workspace tests' 'Run Linux sandbox tests' 'Windows cross-compile check' 'Rust on macOS (deny-warnings build + lint)'; do
      printf '%s\tcompleted\tsuccess\n' "$name"
    done
    ;;
  'issue list'*) [[ "${DUPLICATE:-false}" != true ]] || echo 99 ;;
  'issue create'*) echo https://github.com/fixture/repo/issues/99 ;;
  'pr list'*) : ;;
  'pr create'*) echo https://github.com/fixture/repo/pull/100 ;;
  'pr edit'*|'run rerun'*) : ;;
  *) echo "Unexpected gh call: $*" >&2; exit 9 ;;
esac
SH
cat > "$scratch/bin/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$(cat source)" == good && "$(cat later)" == keep ]]
[[ "$HARN_BRANCH_COMMIT_BRANCH" == "automation/revert-main-$FAILED_SHA" ]]
echo signed-publish >> "$FIXTURE/calls"
SH
chmod +x "$scratch/bin/gh" "$scratch/bin/harn"
export PATH="$scratch/bin:$PATH" FIXTURE="$scratch" GH_REPO=fixture/repo
export GH_TOKEN=fixture-token RUN_ID=42 HARN_BIN="$scratch/bin/harn"
cd "$scratch/repo"
git init -q -b main
git config user.name Fixture
git config user.email fixture@example.com
git config commit.gpgsign false
echo good > source
git add .
git commit -qm base
echo bad > source
git add source
git commit -qm regression
FAILED_SHA=$(git rev-parse HEAD)
export FAILED_SHA
echo keep > later
git add later
git commit -qm subsequent

run_case() {
  : > "$scratch/calls"
  bash "$root/scripts/ci/recover_main_ci.sh" > "$scratch/log" 2>&1
}
absent() {
  if grep -Eq "$1" "$scratch/calls"; then
    echo "Unexpected recovery action: $1" >&2
    exit 1
  fi
}
ATTEMPT=1 OUTCOME=success run_case
absent 'rerun|issue create|signed-publish'
ATTEMPT=1 OUTCOME=failure run_case
grep -Fxq 'run rerun 42 --repo fixture/repo --failed' "$scratch/calls"
absent 'issue create|signed-publish'
ATTEMPT=2 OUTCOME=success run_case
grep -Fq '[CI] Flaky main suite' "$scratch/calls"
absent 'run rerun|signed-publish'
ATTEMPT=2 OUTCOME=failure PARENT_RED=true run_case
grep -Fq '[CI] Persistent main failure' "$scratch/calls"
absent signed-publish
ATTEMPT=2 OUTCOME=failure RETRY_JOB='Windows cross-compile check' run_case
grep -Fq '[CI] Unstable main suite' "$scratch/calls"
absent signed-publish
ATTEMPT=2 OUTCOME=failure DUPLICATE=true run_case
absent 'run rerun|issue create|signed-publish'
if ATTEMPT=2 OUTCOME=success NO_JOBS=true run_case; then
  echo 'No measured failure was accepted as a flake' >&2; exit 1
fi
if ATTEMPT=1 OUTCOME=failure EVENT=pull_request run_case; then
  echo 'Untrusted event was accepted' >&2; exit 1
fi
ATTEMPT=2 OUTCOME=failure run_case
grep -Fxq signed-publish "$scratch/calls"
grep -Fq -- '--add-label ship' "$scratch/calls"
absent 'run rerun'
[[ "$(cat source)" == good && "$(cat later)" == keep ]]
echo 'Main recovery: retry, measured flake, signed culprit revert, duplicate, unknown parent, and untrusted-event controls passed.'
