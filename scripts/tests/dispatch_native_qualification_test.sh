#!/usr/bin/env bash
# Main-policy dispatch selects a source input without creating candidate refs.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
sha=0123456789abcdef0123456789abcdef01234567

mkdir -p "$tmp/bin"
# The full receipt gate is covered by ci_full_suite_test.sh. Here a missing
# successful receipt must dispatch again, while a live exact-source run reuses.
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
case "$*" in
  *workflows/macos-nightly.yml/runs*)
    jq -n --argjson runs "$FAKE_RUNS" '[{total_count:($runs|length),workflow_runs:$runs}]' ;;
  *workflows/macos-nightly.yml*) exit 1 ;;
  "workflow run"*) ;;
  *) echo "unexpected gh $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/gh"

run() {
  : > "$tmp/gh.log"
  set +e
  env PATH="$tmp/bin:$PATH" FAKE_GH_LOG="$tmp/gh.log" \
    GH_REPO=o/r SOURCE_SHA="$sha" FAKE_RUNS="$1" \
    bash "$root/scripts/ci/dispatch_native_qualification.sh" > "$tmp/out" 2>&1
  status=$?
  set -e
}

live=$(jq -nc --arg source "$sha" '[{id:5,head_sha:$source,head_branch:"main",event:"workflow_dispatch",path:".github/workflows/macos-nightly.yml",display_title:("Native qualification " + $source),repository:{full_name:"o/r"},head_repository:{full_name:"o/r"},status:"in_progress",conclusion:null}]')
run "$live"
if [[ $status != 0 ]] || grep -q 'workflow run' "$tmp/gh.log"; then fail "a live run was not reused: $(cat "$tmp/out")"; fi

run "$(jq '.[0].status="completed"|.[0].conclusion="failure"' <<< "$live")"
[[ $status == 0 ]] || fail "a fresh dispatch failed: $(cat "$tmp/out")"
grep -q "^workflow run macos-nightly.yml --repo o/r --ref main -f source_sha=$sha" "$tmp/gh.log" \
  || fail "the qualification was not dispatched from main with the selected source"

run '[]'
if [[ $status != 0 ]] || grep -q 'POST' "$tmp/gh.log" || ! grep -q '^workflow run' "$tmp/gh.log"; then
  fail "an empty census did not dispatch"
fi

run "$(jq '.[0].status="completed"|.[0].conclusion="success"' <<< "$live")"
if [[ $status != 0 ]] || ! grep -q '^workflow run' "$tmp/gh.log"; then fail "a successful title without an authenticated receipt was reused"; fi

# Exercise admission before a candidate can execute on a trusted runner.
mkdir -p "$tmp/repo/scripts/ci" "$tmp/runner"
cp "$root/scripts/ci/native_qualification_probe.sh" "$root/scripts/ci/native_qualification_receipt.sh" "$tmp/repo/scripts/ci/"
cp "$root/scripts/ci/hostlib_macos_stress.sh" "$root/scripts/ci/nextest_inventory.harn" \
  "$root/scripts/ci/summarize_nextest_receipt.harn" "$tmp/repo/scripts/ci/"
cp "$root/scripts/release_audit_contract.json" "$tmp/repo/scripts/"
git -C "$tmp/repo" init -q -b main
fixture_commit() {
  git -C "$tmp/repo" -c user.name='Native fixture' -c user.email=fixture@example.invalid -c commit.gpgsign=false commit -qm "$1"
}
git -C "$tmp/repo" add scripts
fixture_commit 'Fixture reviewed source'
candidate=$(git -C "$tmp/repo" rev-parse HEAD)
printf 'policy\n' > "$tmp/repo/policy"
git -C "$tmp/repo" add policy
fixture_commit 'Fixture trusted policy'
policy=$(git -C "$tmp/repo" rev-parse HEAD)
admit() {
  (cd "$tmp/repo" && env GITHUB_EVENT_NAME=workflow_dispatch GITHUB_REF="$1" GITHUB_SHA="$2" SOURCE_SHA="$3" GITHUB_ENV="$tmp/job-env" RUNNER_TEMP="$tmp/runner" bash "$root/scripts/ci/native_qualification_source.sh")
}
admit refs/heads/main "$policy" "$candidate"
[[ $(cat "$tmp/runner/native-qualification-tools/admitted-source") == "$candidate" ]] || fail 'admitted source was not recorded'
[[ $(cat "$tmp/job-env") == "HARN_BUILD_REVISION=$candidate" ]] || fail 'compiled CLI identity was not bound to admitted source'

# Optional stress proof must be complete before the ordinary exact-source
# receipt is published. Missing evidence cannot inherit an old green receipt.
mkdir -p "$tmp/target/debug"
cat > "$tmp/target/debug/harn" <<'EOF'
#!/usr/bin/env bash
printf '{"data":{"source_revision":"%s"}}\n' "$SOURCE_SHA"
EOF
chmod +x "$tmp/target/debug/harn"
receipt() {
  (cd "$tmp/repo" && env SOURCE_SHA="$candidate" GITHUB_SHA="$policy" \
    GITHUB_REPOSITORY=fixture/repo GITHUB_RUN_ID=81 GITHUB_RUN_ATTEMPT=1 \
    RUNNER_TEMP="$tmp/runner" CARGO_TARGET_DIR="$tmp/target" HOSTLIB_STRESS="$1" \
    bash "$root/scripts/ci/native_qualification_receipt.sh")
}
git -C "$tmp/repo" checkout -q --detach "$candidate"
receipt false
rm "$tmp/runner/native-qualification-tools/receipt.json"
if receipt unknown; then fail 'an unknown stress-proof mode published a receipt'; fi
if receipt true; then fail 'missing stress evidence published a qualification receipt'; fi
stress="$tmp/runner/native-qualification-tools/hostlib-stress"
mkdir -p "$stress"
printf '4\n' > "$stress/selected-count.txt"
complete='{"status":0,"threads":32,"stress_count":20,"receipt":{"reason":"complete","selected":80,"run":80,"passed":80,"failed":0,"missing":[]}}'
printf '%s\n' "$complete" > "$stress/summary.json"
receipt true
rm "$tmp/runner/native-qualification-tools/receipt.json"
for defect in '.stress_count=19' '.threads=31' '.status=70' '.receipt.run=79' \
  '.receipt.passed=79|.receipt.failed=1' '.receipt.missing=["not-run"]' \
  '.receipt.selected=0' '.receipt.reason="tests-not-run"' \
  '.receipt.selected=100|.receipt.run=100|.receipt.passed=100'; do
  jq "$defect" <<< "$complete" > "$stress/summary.json"
  if receipt true; then fail "incomplete stress evidence accepted: $defect"; fi
  [[ ! -e "$tmp/runner/native-qualification-tools/receipt.json" ]] || fail 'failed stress proof wrote a receipt'
done
git -C "$tmp/repo" checkout -q --detach "$policy"
if admit refs/heads/topic "$policy" "$candidate"; then fail 'untrusted workflow ref was admitted'; fi
if admit refs/heads/main "$candidate" "$candidate"; then fail 'workflow checkout mismatch was admitted'; fi
printf 'unreviewed\n' > "$tmp/repo/topic"
git -C "$tmp/repo" add topic
fixture_commit 'Fixture unreviewed descendant'
unreviewed=$(git -C "$tmp/repo" rev-parse HEAD)
git -C "$tmp/repo" checkout -q --detach "$policy"
if admit refs/heads/main "$policy" "$unreviewed"; then fail 'source outside reviewed main ancestry was admitted'; fi
if admit refs/heads/main "$policy" invalid; then fail 'invalid source was admitted'; fi
echo "dispatch_native_qualification_test: dispatch and reviewed-source admission controls pass"
