#!/usr/bin/env bash
# The owned macOS check is satisfied only by a success whose run is the trusted one for this pull
# request and SHA; it is not required when no changed file is read by the tests; a missing check
# waits, then fails. Every other state is a non-pass.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"

repo="burin-labs/harn"
pr=77
sha="1111111111111111111111111111111111111111"
other="2222222222222222222222222222222222222222"
name="macOS workspace tests (owned)"
now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
old="$(date -u -v-5H +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '5 hours ago' +%Y-%m-%dT%H:%M:%SZ)"

# The stub serves a fixture per endpoint, ignoring paging flags.
cat > "$tmp/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$2" in
  */pulls/*/files*) cat "$FIXTURES/files.json" ;;
  */commits/*/check-runs*) cat "$FIXTURES/checks.json" ;;
  */actions/runs/*) cat "$FIXTURES/run.json" ;;
  *) echo "unexpected gh $*" >&2; exit 1 ;;
esac
SH
chmod +x "$tmp/bin/gh"
export FIXTURES="$tmp"

decide() {
  local expect_code="$1" label="$2"
  set +e
  PATH="$tmp/bin:$PATH" "$root/scripts/ci/owned_macos_pr_check.sh" "$repo" "$pr" "$sha" > "$tmp/out" 2>&1
  local code=$?
  set -e
  if [[ "$code" != "$expect_code" ]]; then
    echo "expected exit $expect_code, got $code: $label" >&2
    cat "$tmp/out" >&2
    exit 1
  fi
}

files() { printf '[%s]' "$(printf '{"filename":"%s"},' "$@" | sed 's/,$//')" > "$tmp/files.json"; }
check() { # id status conclusion head_sha started
  printf '{"check_runs":[{"id":%s,"name":"%s","head_sha":"%s","status":"%s","conclusion":%s,"started_at":"%s","details_url":"https://github.com/%s/actions/runs/900"}]}' \
    "$1" "$name" "$4" "$2" "$3" "$5" "$repo" > "$tmp/checks.json"
}
run() { # path event branch title status conclusion
  printf '{"path":"%s","event":"%s","head_branch":"%s","repository":{"full_name":"%s"},"display_title":"%s","status":"%s","conclusion":"%s"}' \
    "$1" "$2" "$3" "$repo" "$4" "$5" "$6" > "$tmp/run.json"
}
good_title="macOS PR tests for #${pr} at ${sha}"
good_run() { run .github/workflows/macos-pr-tests.yml workflow_dispatch main "$good_title" completed success; }

# Pull requests that touch nothing the tests read need no check, and say so.
files README.md docs/guide.md
echo '{"check_runs":[]}' > "$tmp/checks.json"
decide 0 "docs-only pull request is not required"
grep -q "not required" "$tmp/out" || { echo "the not-required pass must be named" >&2; exit 1; }

# Pull requests that do touch them need the check.
files docs/guide.md crates/harn-vm/src/lib.rs
good_run
check 1 completed '"success"' "$sha" "$now"
decide 0 "a success from the trusted run"
for relevant in Cargo.toml Cargo.lock .config/nextest.toml scripts/ci/run_rust_test_lane.sh .github/actions/rust-toolchain/action.yml; do
  files "$relevant"
  decide 0 "$relevant is read by the tests and the check is a success"
  grep -q "success on" "$tmp/out" || { echo "$relevant must be treated as required" >&2; exit 1; }
done
files docs/guide.md crates/harn-vm/src/lib.rs

# A missing check waits while the commit is young and fails once it is old.
echo '{"check_runs":[{"id":5,"name":"other","head_sha":"'"$sha"'","status":"completed","conclusion":"success","started_at":"'"$now"'"}]}' > "$tmp/checks.json"
decide 3 "missing check, young commit"
echo '{"check_runs":[{"id":5,"name":"other","head_sha":"'"$sha"'","status":"completed","conclusion":"success","started_at":"'"$old"'"}]}' > "$tmp/checks.json"
decide 1 "missing check, old commit"
echo '{"check_runs":[]}' > "$tmp/checks.json"
decide 3 "no checks at all yet"

# The check itself.
check 1 in_progress null "$sha" "$now";                 decide 3 "still running"
check 1 in_progress null "$sha" "$old";                 decide 1 "still running past the limit"
check 1 completed '"failure"' "$sha" "$now";            decide 1 "failure"
check 1 completed '"neutral"' "$sha" "$now";            decide 1 "neutral"
check 1 completed '"success"' "$other" "$now";          decide 3 "a success recorded on another SHA does not count"

# The run behind a success must be the trusted one.
check 1 completed '"success"' "$sha" "$now"
run .github/workflows/other.yml workflow_dispatch main "$good_title" completed success;                        decide 1 "another workflow"
run .github/workflows/macos-pr-tests.yml push main "$good_title" completed success;                            decide 1 "not a dispatch"
run .github/workflows/macos-pr-tests.yml workflow_dispatch feature "$good_title" completed success;            decide 1 "a branch's copy of the workflow"
run .github/workflows/macos-pr-tests.yml workflow_dispatch main "macOS PR tests for #78 at ${sha}" completed success; decide 1 "another pull request"
run .github/workflows/macos-pr-tests.yml workflow_dispatch main "macOS PR tests for #${pr} at ${other}" completed success; decide 1 "another SHA"
run .github/workflows/macos-pr-tests.yml workflow_dispatch main "$good_title" in_progress null;                decide 1 "a run that has not finished"
sed -i.bak "s#github.com/${repo}/actions#github.com/someone/else/actions#" "$tmp/checks.json"
good_run;                                                                                                       decide 1 "a check pointing at another repository"

# An unreadable file list is not a pass.
echo '[]' > "$tmp/files.json"
check 1 completed '"success"' "$sha" "$now"
decide 1 "no files read"

# A branch name is not a SHA.
if PATH="$tmp/bin:$PATH" "$root/scripts/ci/owned_macos_pr_check.sh" "$repo" "$pr" main >/dev/null 2>&1; then
  echo "a branch name must not be accepted as a SHA" >&2
  exit 1
fi
echo "owned macOS PR check: ok"
