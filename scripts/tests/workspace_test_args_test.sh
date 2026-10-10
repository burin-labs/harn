#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/repo/scripts/ci" "$scratch/repo/scripts/config"
cp "$root/scripts/ci/affected_crate_args.sh" "$scratch/repo/scripts/ci/"
cp "$root/scripts/config/affected-crate-global-paths.txt" "$scratch/repo/scripts/config/"
cd "$scratch/repo"
git init -q -b main
git config user.name Fixture
git config user.email fixture@example.com
git config commit.gpgsign false
cat > Cargo.toml <<'TOML'
[workspace]
members = ["leaf", "consumer", "unrelated"]
resolver = "2"
TOML
for name in leaf consumer unrelated; do
  mkdir -p "$name/src"
  printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2021"\n' "$name" > "$name/Cargo.toml"
  printf 'pub fn value() {}\n' > "$name/src/lib.rs"
done
printf '\n[dependencies]\nleaf = { path = "../leaf" }\n' >> consumer/Cargo.toml
git add .
git commit -qm base
base=$(git rev-parse HEAD)
printf 'pub fn changed() {}\n' >> leaf/src/lib.rs
git add .
git commit -qm changed
head=$(git rev-parse HEAD)
jq -n --arg base "$base" --arg head "$head" '{pull_request: {base: {sha: $base}}, merge_group: {base_sha: $base, head_sha: $head}}' > "$scratch/event"
export GITHUB_EVENT_PATH="$scratch/event"
for event in pull_request merge_group; do
  actual=$(GITHUB_EVENT_NAME="$event" POST_MERGE_TIER_ACTIVE=true bash "$root/scripts/ci/workspace_test_args.sh")
  [[ "$actual" == '-p consumer -p leaf' ]] || { echo "Wrong affected selection: $actual" >&2; exit 1; }
  actual=$(GITHUB_EVENT_NAME="$event" POST_MERGE_TIER_ACTIVE=false bash "$root/scripts/ci/workspace_test_args.sh")
  [[ "$actual" == --workspace ]]
done
actual=$(GITHUB_EVENT_NAME=push POST_MERGE_TIER_ACTIVE=true bash "$root/scripts/ci/workspace_test_args.sh")
[[ "$actual" == --workspace ]]
jq -n '{pull_request: {base: {sha: "missing"}}}' > "$scratch/event"
if GITHUB_EVENT_NAME=pull_request POST_MERGE_TIER_ACTIVE=true bash "$root/scripts/ci/workspace_test_args.sh" > "$scratch/log" 2>&1; then
  echo 'An unreadable diff base selected passing empty coverage' >&2; exit 1
fi
mkdir "$scratch/bin"
cat > "$scratch/bin/cargo-nextest" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'nextest list '*)
    jq -n --arg status "${MATCH_STATUS:-matches}" '{"rust-suites": {fixture: {testcases: {case: {ignored: false, "filter-match": {status: $status}}}}}}'
    ;;
  'nextest run '*) printf '%s\n' "$*" > "$RAN_TESTS" ;;
  *) exit 9 ;;
esac
SH
chmod +x "$scratch/bin/cargo-nextest"
export PATH="$scratch/bin:$PATH" RAN_TESTS="$scratch/ran"
GITHUB_EVENT_NAME=push bash "$root/scripts/ci/run_workspace_tests.sh" count:3/3 > "$scratch/log"
grep -q 'eligible=1' "$scratch/log"
grep -q -- '--partition count:3/3 --no-tests pass' "$scratch/ran"
rm "$scratch/ran"
if MATCH_STATUS=mismatch GITHUB_EVENT_NAME=push bash "$root/scripts/ci/run_workspace_tests.sh" count:3/3 > "$scratch/log" 2>&1; then
  echo 'An empty eligible suite was accepted' >&2; exit 1
fi
[[ ! -e "$scratch/ran" ]]
echo 'Workspace test selection: PR and merge-group reverse dependencies agree; main and full scope use the whole workspace; invalid scope fails.'
