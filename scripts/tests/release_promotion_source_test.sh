#!/usr/bin/env bash
# Exercise the workflow's real source resolver, including unread/partial data.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$root/scripts/lib/candidate_archive_contract.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
sha=8743d2bd57df14eb1a022dec9cb842cbcbd7b568
jq -n --arg sha "$sha" '{id:4242, repository:{id:1,full_name:"burin-labs/harn"},
  head_repository:{id:1,full_name:"burin-labs/harn"},
  path:".github/workflows/build-release-binaries.yml", status:"completed",
  conclusion:"success", event:"merge_group", head_sha:$sha}' > "$tmp/run.json"
jq -n --arg sha "$sha" '{status:"ahead", merge_base_commit:{sha:$sha}}' > "$tmp/compare.json"
printf '[workspace.package]\nversion = "0.10.159"\n' > "$tmp/Cargo.toml"
candidate_archive_expected_targets_json | jq --arg sha "$sha" --arg files "$RELEASE_FILES_ARTIFACT" '
  [.[] | "harn-" + .] + ["candidate-manifest-" + $sha, $files] |
  {total_count:length, artifacts:to_entries | map({id:(.key + 1),name:.value,
    expired:false,size_in_bytes:100,workflow_run:{id:4242,repository_id:1,
      head_repository_id:1,head_sha:$sha,head_branch:"main"}})}' > "$tmp/artifacts.json"
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$2" in
  repos/burin-labs/harn/actions/runs/4242) cat "$FIXTURE/run.json" ;;
  'repos/burin-labs/harn/actions/runs/4242/artifacts?per_page=100') cat "$FIXTURE/artifacts.json" ;;
  repos/burin-labs/harn/contents/Cargo.toml\?ref=*) cat "$FIXTURE/Cargo.toml" ;;
  repos/burin-labs/harn/compare/*...main) cat "$FIXTURE/compare.json" ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/gh"
resolve() {
  : > "$tmp/output"
  env PATH="$tmp/bin:$PATH" FIXTURE="$tmp" GITHUB_REPOSITORY=burin-labs/harn \
    CANDIDATE_RUN_ID="${1:-4242}" GITHUB_OUTPUT="$tmp/output" \
    EXPECTED_SOURCE_SHA="${2:-$sha}" \
    bash "${RELEASE_PROMOTION_SOURCE_SCRIPT:-$root/scripts/resolve-release-promotion-source.sh}" > "$tmp/log" 2>&1
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
jq '.event="workflow_dispatch" | .head_branch="main"' "$tmp/good-run.json" > "$tmp/run.json"
cp "$tmp/run.json" "$tmp/good-manual.json"
cp "$tmp/artifacts.json" "$tmp/good-artifacts.json"
resolve || fail "successful stable manual main candidate refused"
grep -Fxq "source_sha=$sha" "$tmp/output" || fail "wrong manual source"
for mutation in '.head_branch="feature"' '.event="schedule"' '.event="pull_request"'; do
  jq "$mutation" "$tmp/good-manual.json" > "$tmp/run.json"
  refuse "manual producer $mutation"
done
cp "$tmp/good-manual.json" "$tmp/run.json"
for version in 0.10.159-dev 0.10.159-rc.1 0.010.159 invalid ''; do
  printf '[workspace.package]\nversion = "%s"\n' "$version" > "$tmp/Cargo.toml"
  refuse "manual candidate version $version"
done
printf '[workspace.package]\nversion = "0.10.159"\n' > "$tmp/Cargo.toml"
for mutation in '.total_count=8' '.artifacts=[] | .total_count=0' \
  '.artifacts[0].expired=true' '.artifacts[0].size_in_bytes=0' \
  '.artifacts[0].size_in_bytes="100"' '.artifacts[0].workflow_run.id=9999' \
  '.artifacts[0].workflow_run.head_sha="0000000000000000000000000000000000000000"' \
  '.artifacts[0].workflow_run.head_branch="feature"' \
  '.artifacts[0].workflow_run.repository_id=2' '.artifacts[0].workflow_run.head_repository_id=2' \
  'del(.artifacts[0].workflow_run)' '.artifacts[0].name="unrelated"' \
  '.artifacts[0].name=.artifacts[1].name' 'del(.artifacts[-1]) | .total_count-=1' '{}'; do
  jq "$mutation" "$tmp/good-artifacts.json" > "$tmp/artifacts.json"
  refuse "manual artifacts $mutation"
done
printf 'not json\n' > "$tmp/artifacts.json"
refuse 'malformed manual artifacts'
: > "$tmp/artifacts.json"
refuse 'empty manual artifacts'
cp "$tmp/good-artifacts.json" "$tmp/artifacts.json"
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
if resolve 4242 0000000000000000000000000000000000000000; then
  fail 'producer differing from publication key accepted'
fi
[[ ! -s "$tmp/output" ]] || fail 'mismatched publication key emitted output'
if resolve 4242 abc; then fail 'invalid publication key accepted'; fi
echo 'release_promotion_source_test: ok'
