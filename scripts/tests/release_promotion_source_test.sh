#!/usr/bin/env bash
# Exercise the workflow's real source resolver, including unread/partial data.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
sha=8743d2bd57df14eb1a022dec9cb842cbcbd7b568
jq -n --arg sha "$sha" '{id:4242, repository:{full_name:"burin-labs/harn"},
  head_repository:{full_name:"burin-labs/harn"},
  path:".github/workflows/build-release-binaries.yml", status:"completed",
  conclusion:"success", event:"merge_group", head_sha:$sha}' > "$tmp/run.json"
jq -n --arg sha "$sha" '{status:"ahead", merge_base_commit:{sha:$sha}}' > "$tmp/compare.json"
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$2" in
  repos/burin-labs/harn/actions/runs/4242) cat "$FIXTURE/run.json" ;;
  repos/burin-labs/harn/compare/*...main) cat "$FIXTURE/compare.json" ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/gh"
resolve() {
  : > "$tmp/output"
  env PATH="$tmp/bin:$PATH" FIXTURE="$tmp" GITHUB_REPOSITORY=burin-labs/harn \
    CANDIDATE_RUN_ID="${1:-4242}" GITHUB_OUTPUT="$tmp/output" \
    bash "$root/scripts/resolve-release-promotion-source.sh" > "$tmp/log" 2>&1
}
fail() { echo "FAIL: $*" >&2; exit 1; }
refuse() {
  if resolve; then fail "$1 accepted"; fi
  [[ ! -s "$tmp/output" ]] || fail "$1 emitted a source"
}
cp "$tmp/run.json" "$tmp/good-run.json"
cp "$tmp/compare.json" "$tmp/good-compare.json"
resolve || fail "successful landed merge-group candidate refused"
grep -Fxq "source_sha=$sha" "$tmp/output" || fail "wrong source"
grep -Fxq 'run_id=4242' "$tmp/output" || fail "wrong producer"
jq '.event="push" | .head_branch="main"' "$tmp/good-run.json" > "$tmp/run.json"
resolve || fail "successful main push refused"
for mutation in '.conclusion="failure"' '.status="in_progress"' '.event="pull_request"' \
  '.repository.full_name="foreign/repo"' '.head_repository.full_name="fork/harn"' \
  '.path=".github/workflows/ci.yml"' '.head_sha="abc"' '.id=9999' \
  'del(.status)' 'del(.head_sha)' '{}'; do
  jq "$mutation" "$tmp/good-run.json" > "$tmp/run.json"
  refuse "producer $mutation"
done
jq '.event="push" | .head_branch="feature"' "$tmp/good-run.json" > "$tmp/run.json"
refuse 'non-main push'
cp "$tmp/good-run.json" "$tmp/run.json"
for mutation in '.status="diverged"' '.status="behind"' \
  '.merge_base_commit.sha="0000000000000000000000000000000000000000"' '{}'; do
  jq "$mutation" "$tmp/good-compare.json" > "$tmp/compare.json"
  refuse "comparison $mutation"
done
: > "$tmp/compare.json"
refuse 'empty comparison'
printf 'not json\n' > "$tmp/compare.json"
refuse 'malformed comparison'
cp "$tmp/good-compare.json" "$tmp/compare.json"
: > "$tmp/run.json"
refuse 'empty producer'
cp "$tmp/good-run.json" "$tmp/run.json"
if resolve '4242/../../other'; then fail 'invalid run accepted'; fi
[[ ! -s "$tmp/output" ]] || fail 'invalid run emitted output'
echo 'release_promotion_source_test: ok'
