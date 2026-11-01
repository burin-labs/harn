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
    printf '%s %s|%s\n' "$(git rev-parse HEAD)" "${RUSTUP_TOOLCHAIN:-unset}" "${RUSTFLAGS:-}" \
      >> "$TEST_DOC_TOOLCHAINS"
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
cat > "$tmp/bin/rustup" <<'RUSTUP'
#!/usr/bin/env bash
[[ "$*" == 'show active-toolchain' ]] || exit 2
printf 'head-toolchain (overridden by rust-toolchain.toml)\n'
RUSTUP
chmod +x "$tmp/bin/cargo" "$tmp/bin/rustc" "$tmp/bin/rustup"
export PATH="$tmp/bin:$PATH" TEST_TARGET="$tmp/target" TEST_DOC_CALLS="$tmp/doc-calls"
export TEST_DOC_TOOLCHAINS="$tmp/doc-toolchains"
# CI jobs export RUSTFLAGS (and may export RUSTUP_TOOLCHAIN); the recorded
# flags below assert only what the gate itself adds.
unset RUSTFLAGS RUSTUP_TOOLCHAIN
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
  : > "$TEST_DOC_TOOLCHAINS"
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
# A merge base with the same toolchain pin keeps the job's flags and compiler choice.
grep -qx "$base unset|" "$TEST_DOC_TOOLCHAINS"
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

# A toolchain move: the merge base pins an older compiler whose rustdoc JSON
# format the current side cannot be compared with, so both sides use the job's.
printf '[toolchain]\nchannel = "old"\n' > rust-toolchain.toml
git add rust-toolchain.toml
git commit -qm 'old toolchain'
base=$(git rev-parse HEAD)
printf '[toolchain]\nchannel = "new"\n' > rust-toolchain.toml
git add rust-toolchain.toml
git commit -qm 'new toolchain'
head=$(git rev-parse HEAD)
run_gate "$tmp/missing"
grep -qx "$base head-toolchain| --cap-lints=warn" "$TEST_DOC_TOOLCHAINS"
grep -qx "$head unset|" "$TEST_DOC_TOOLCHAINS"
grep -q 'pins another Rust toolchain; documenting it with head-toolchain' "$tmp/result"
echo 'Public API rustdoc: exact reuse avoids baseline compilation; foreign compiler, corrupt files, and empty census rebuild both sides; a toolchain move documents the baseline with the current compiler.'
