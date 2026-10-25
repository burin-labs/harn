#!/usr/bin/env bash
# Reach the actual gate's cold, reused, invalidated, and incomplete paths.
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
gate="$repo_root/.github/scripts/breaking-surface-check.sh"
source "$repo_root/scripts/lib/sha256.sh"
source "$repo_root/.github/scripts/lib/public-api-rustdoc.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/repo" "$tmp/target/doc"
cat > "$tmp/bin/rustc" <<'RUSTC'
#!/usr/bin/env bash
[[ ${TEST_COMPILER:-} != unavailable ]] || exit 1
printf 'rustc test compiler %s\n' "${TEST_COMPILER:-one}"
RUSTC
cat > "$tmp/bin/cargo" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  metadata)
    jq -n --arg target "$TEST_TARGET" '{target_directory: $target, packages: [
      {name: "harn-alpha", publish: null, targets: [{name: "harn_alpha", kind: ["lib"]}]},
      {name: "harn-beta", publish: null, targets: [{name: "harn_beta", kind: ["lib"]}]}]}'
    ;;
  doc)
    git rev-parse HEAD >> "$TEST_DOC_CALLS"
    mkdir -p "$TEST_TARGET/doc"
    printf '{"api": "alpha"}\n' > "$TEST_TARGET/doc/harn_alpha.json"
    printf '{"api": "beta"}\n' > "$TEST_TARGET/doc/harn_beta.json"
    ;;
  semver-checks)
    printf '    Checking <unknown> v1 -> v1\n     Checked 196 checks: 196 pass\n     Summary no semver update required\n'
    ;;
  *) exit 2 ;;
esac
CARGO
chmod +x "$tmp/bin/cargo" "$tmp/bin/rustc"
export PATH="$tmp/bin:$PATH" TEST_TARGET="$tmp/target" TEST_DOC_CALLS="$tmp/doc-calls"
cd "$tmp/repo"
git init -q -b main
git config user.name 'Harn Test'
git config user.email 'harn-test@example.invalid'
git config commit.gpgsign false
git config maintenance.auto false
printf 'baseline\n' > README
git add README
git commit -qm baseline
base=$(git rev-parse HEAD)
publishable_rustdoc "$tmp/baseline"
public_api_rustdoc_valid "$tmp/baseline" "$base"
if TEST_COMPILER=unavailable public_api_rustdoc_valid "$tmp/baseline" "$base"; then
  echo 'Missing compiler identity was accepted.' >&2
  exit 1
fi
printf 'current\n' > README
git add README
git commit -qm current
head=$(git rev-parse HEAD)

run_gate() {
  : > "$TEST_DOC_CALLS"
  BASE_SHA="$base" HEAD_SHA=HEAD PUBLIC_API_RUSTDOC_BASELINE="$1" \
    PUBLIC_API_RUSTDOC_OUTPUT="$tmp/output" bash "$gate" rust-api > "$tmp/result" 2>&1
  [[ $(git rev-parse HEAD) == "$head" ]]
  [[ $(grep -c '^[[:space:]]*Checking ' "$tmp/result") == 2 ]]
  grep -q 'no rust-api break; pass' "$tmp/result"
  public_api_rustdoc_valid "$tmp/output" "$head"
}

run_gate "$tmp/missing"
[[ $(wc -l < "$TEST_DOC_CALLS") -eq 2 ]]
grep -qx "$head" "$TEST_DOC_CALLS"
grep -qx "$base" "$TEST_DOC_CALLS"
run_gate "$tmp/baseline"
[[ $(wc -l < "$TEST_DOC_CALLS") -eq 1 ]]
grep -qx "$head" "$TEST_DOC_CALLS"
grep -q 'Reused exact baseline rustdoc' "$tmp/result"

run_gate "$tmp/output"
[[ $(wc -l < "$TEST_DOC_CALLS") -eq 2 ]]
TEST_COMPILER=two run_gate "$tmp/baseline"
[[ $(wc -l < "$TEST_DOC_CALLS") -eq 2 ]]
printf '{"api": "corrupted"}\n' > "$tmp/baseline/harn_beta.json"
run_gate "$tmp/baseline"
[[ $(wc -l < "$TEST_DOC_CALLS") -eq 2 ]]
: > "$tmp/baseline/crates.tsv"
run_gate "$tmp/baseline"
[[ $(wc -l < "$TEST_DOC_CALLS") -eq 2 ]]
echo 'Public API rustdoc: exact reuse avoids baseline compilation; foreign compiler, corrupt files, and empty census rebuild both sides.'
