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
    printf '%s\n' "$*" > "$LISTED_TESTS"
    jq -n --arg status "${MATCH_STATUS:-matches}" '{"test-count": 1, "rust-suites": {fixture: {status: "listed", "package-name": "fixture", "binary-name": "fixture", testcases: {case: {ignored: false, "filter-match": {status: $status}}}}, excluded: {status: "skipped"}}}'
    ;;
  'nextest run '*) printf '%s\n' "$*" > "$RAN_TESTS" ;;
  *) exit 9 ;;
esac
SH
chmod +x "$scratch/bin/cargo-nextest"
# The vm group reads harn-vm's workspace-resolved features from cargo metadata.
# Other callers (the affected-crate graph) keep the real cargo.
REAL_CARGO=$(command -v cargo)
export REAL_CARGO
cat > "$scratch/bin/cargo" <<'SH'
#!/usr/bin/env bash
[[ "${FAKE_VM_METADATA:-}" == 1 && "$1" == metadata ]] || exec "$REAL_CARGO" "$@"
jq -n '{packages: [{name: "harn-vm", source: null, id: "vm-id"}, {name: "harn-vm", source: "registry", id: "other"}],
  resolve: {nodes: [{id: "vm-id", features: ["default", "otel"]}, {id: "other", features: ["wrong"]}]}}'
SH
chmod +x "$scratch/bin/cargo"
export PATH="$scratch/bin:$PATH" RAN_TESTS="$scratch/ran" LISTED_TESTS="$scratch/listed"
export RUNNER_TEMP="$scratch"
FAKE_VM_METADATA=1 GITHUB_EVENT_NAME=push bash "$root/scripts/ci/run_workspace_tests.sh" vm:3/3 > "$scratch/log"
jq -e '.["rust-suites"].fixture.testcases.case["filter-match"].status == "matches"' "$RUNNER_TEMP/harn-nextest-inventory.json" > /dev/null
grep -q 'eligible=1' "$scratch/log"
grep -q -- '--locked -p harn-vm --features harn-vm/default,harn-vm/otel --partition count:3/3 --no-tests pass' "$scratch/ran"
GITHUB_EVENT_NAME=push bash "$root/scripts/ci/run_workspace_tests.sh" rest:1/5 > "$scratch/log"
grep -q -- '--locked --workspace --exclude harn-vm --partition count:1/5 --no-tests pass' "$scratch/ran"
# The producer keeps the unified workspace build and filters harn-vm out instead.
GITHUB_EVENT_NAME=push bash "$root/scripts/ci/run_workspace_tests.sh" rest-unified:1/5 > "$scratch/log"
grep -q -- 'and not package(harn-vm) --locked --workspace --partition count:1/5 --no-tests pass' "$scratch/ran"
if grep -q -- '--exclude' "$scratch/ran"; then
  echo 'The unified producer share changed the package selection' >&2; exit 1
fi
# An affected selection without harn-vm leaves the vm group nothing to build.
jq -n --arg base "$base" '{pull_request: {base: {sha: $base}}}' > "$scratch/event"
rm "$scratch/ran"
GITHUB_EVENT_NAME=pull_request POST_MERGE_TIER_ACTIVE=true bash "$root/scripts/ci/run_workspace_tests.sh" vm:1/3 > "$scratch/log"
grep -q 'group=vm selects nothing' "$scratch/log"
[[ ! -e "$scratch/ran" ]]
GITHUB_EVENT_NAME=pull_request POST_MERGE_TIER_ACTIVE=true bash "$root/scripts/ci/run_workspace_tests.sh" rest:2/5 > "$scratch/log"
grep -q -- '--locked -p consumer -p leaf --partition count:2/5' "$scratch/ran"
if bash "$root/scripts/ci/run_workspace_tests.sh" count:1/3 > "$scratch/log" 2>&1; then
  echo 'An ungrouped partition was accepted' >&2; exit 1
fi
rm "$scratch/ran"
if FAKE_VM_METADATA=1 MATCH_STATUS=mismatch GITHUB_EVENT_NAME=push bash "$root/scripts/ci/run_workspace_tests.sh" vm:3/3 > "$scratch/log" 2>&1; then
  echo 'An empty eligible suite was accepted' >&2; exit 1
fi
[[ ! -e "$scratch/ran" ]]
jq -e '.["rust-suites"].fixture.testcases.case["filter-match"].status == "mismatch"' "$RUNNER_TEMP/harn-nextest-inventory.json" > /dev/null
GITHUB_WORKSPACE="$scratch/repo" bash "$root/scripts/ci/run_workspace_tests.sh" all "$scratch/tests.tar.zst" > "$scratch/log"
grep -Fq -- "--archive-file $scratch/tests.tar.zst --workspace-remap $scratch/repo" "$scratch/listed"
grep -q -- '--cargo-metadata .*/target/nextest/cargo-metadata.json' "$scratch/ran"
grep -q -- '--binaries-metadata .*/target/nextest/binaries-metadata.json' "$scratch/ran"
grep -Fq -- "--workspace-remap $scratch/repo" "$scratch/ran"
grep -q -- '--no-tests fail' "$scratch/ran"
if grep -Eq -- '--locked|--partition|--archive-file' "$scratch/ran"; then
  echo 'Archive replay rebuilt, partitioned, or unpacked the archive twice' >&2; exit 1
fi
rm "$scratch/ran"
if MATCH_STATUS=mismatch GITHUB_WORKSPACE="$scratch/repo" bash "$root/scripts/ci/run_workspace_tests.sh" all "$scratch/tests.tar.zst" > "$scratch/log" 2>&1; then
  echo 'An empty archive selection was accepted' >&2; exit 1
fi
[[ ! -e "$scratch/ran" ]]
echo 'Workspace test selection: PR and merge-group reverse dependencies agree; main and full scope use the whole workspace; invalid scope fails.'
