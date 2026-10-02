#!/usr/bin/env bash
# Exercise scripts/ci/main_health_carry.sh against a stubbed `gh`.
#
# A push carries the replaced commit's `main health` status only when nothing
# it changed could alter the verdict and its changed files were fully listed.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/main_health_carry.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

# CHANGED: newline-separated files the push changed. COMPARE_FAIL: make the
# comparison fail. STATUS: the replaced commit's main health "state<TAB>desc".
cat > "$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
args="$*"
case "$args" in
  *"/compare/"*)
    [[ -z "${COMPARE_FAIL:-}" ]] || exit 1
    printf 'count %s\n' "$(grep -c . <<< "${CHANGED:-}" || true)"
    printf '%b' "${CHANGED:-}" ;;
  *"/commits/"*"/statuses"*) printf '%b' "${STATUS:-}" ;;
  *"/statuses/"*) echo "POST $args" >> "$POSTS" ;;
  *) echo "unexpected gh call: $args" >&2; exit 99 ;;
esac
EOF
chmod +x "$work/bin/gh"

failures=0
run_case() { # name expected-carried [env assignments...]
  local name="$1" expected="$2"
  shift 2
  : > "$work/output"
  : > "$work/posts"
  env PATH="$work/bin:$PATH" GH_REPO=example/repo PUSHED_SHA=new REPLACED_SHA=old \
    GITHUB_OUTPUT="$work/output" GITHUB_STEP_SUMMARY="$work/summary" POSTS="$work/posts" \
    STATUS=$'success\told verdict\t\n' "$@" bash "$script" > "$work/out" 2>&1 || true
  local carried posted=false
  carried="$(sed -n 's/^carried=//p' "$work/output")"
  [[ ! -s "$work/posts" ]] || posted=true
  # A carry posts the status; a decline posts nothing.
  if [[ "$carried" == "$expected" && "$posted" == "$expected" ]]; then
    echo "ok   $name -> carried=$carried"
  else
    echo "FAIL $name: expected carried=$expected, got carried=${carried:-nothing} posted=$posted"
    sed 's/^/     /' "$work/out"
    failures=$((failures + 1))
  fi
}

run_case "an unrelated push carries" true CHANGED=$'src/a.rs\n'
run_case "a push changing the registry measures" false \
  CHANGED=$'src/a.rs\nscripts/scheduled_workflows.toml\n'
run_case "a push changing the reader measures" false CHANGED=$'scripts/ci/main_health.sh\n'
run_case "a push changing the workflow measures" false CHANGED=$'.github/workflows/main-health.yml\n'
run_case "an unlistable push measures" false CHANGED=$'src/a.rs\n' COMPARE_FAIL=1
run_case "a push at the compare file cap measures" false \
  CHANGED="$(seq -f 'f%g.txt' 1 300)"
run_case "a replaced commit without a status measures" false CHANGED=$'src/a.rs\n' STATUS=
run_case "a first push measures" false REPLACED_SHA=0000000000000000000000000000000000000000

if (( failures > 0 )); then
  echo "main health carry: $failures case(s) failed"
  exit 1
fi
echo "main health carry: all cases passed"
