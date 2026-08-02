#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
# shellcheck source=scripts/lib/package_verify_bootstrap.sh
source "$repo_root/scripts/lib/package_verify_bootstrap.sh"

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/repo"
mkdir -p "$fixture/scripts/lib" "$fixture/target/debug" "$tmp_root/bin"

cat > "$fixture/scripts/lib/cargo_env.sh" <<'SH'
#!/bin/sh
harn_cargo_metadata_target_dir() {
  printf '%s\n' "$CARGO_TARGET_DIR"
}
SH

fake_harn="$fixture/target/debug/harn"
printf '#!/usr/bin/env bash\nexit 0\n' > "$fake_harn"
chmod +x "$fake_harn"

cat > "$fixture/scripts/harn_bin.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'resolve cwd=%s no_build=%s explicit=%s args=%s\n' \
  "$PWD" "${HARN_BIN_NO_BUILD:-}" "${HARN_BIN:-}" "$*" >> "$CALLS_FILE"
printf '%s\n' "${HARN_BIN:-$FAKE_HARN}"
SH
chmod +x "$fixture/scripts/harn_bin.sh"

cat > "$fixture/scripts/cargo_with_worktree_build_dir.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo cwd=%s args=%s\n' "$PWD" "$*" >> "$CALLS_FILE"
SH
chmod +x "$fixture/scripts/cargo_with_worktree_build_dir.sh"

cat > "$fixture/target/debug/harn-cli-aot-gen" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'aot %s\n' "$*" >> "$CALLS_FILE"
if [[ " ${*} " != *" --check "* ]]; then
  mkdir -p "$FIXTURE_ROOT/crates/harn-cli/generated/cli-bytecode"
  printf '{}\n' > "$FIXTURE_ROOT/crates/harn-cli/generated/cli-bytecode-manifest.json"
else
  [[ -f "$FIXTURE_ROOT/crates/harn-cli/generated/cli-bytecode-manifest.json" ]]
  [[ -d "$FIXTURE_ROOT/crates/harn-cli/generated/cli-bytecode" ]]
fi
SH
chmod +x "$fixture/target/debug/harn-cli-aot-gen"

export CALLS_FILE="$tmp_root/calls"
export FAKE_HARN="$fake_harn"
export FIXTURE_ROOT="$fixture"
export PATH="$tmp_root/bin:$PATH"
export CARGO_TARGET_DIR="$fixture/target"

package_verify_prepare_tools "$fixture"
[[ "$HARN_BIN" == "$fake_harn" ]]

expected="cargo cwd=$fixture args=build -p harn-cli --bin harn -p harn-cli-aot-gen --bin harn-cli-aot-gen
resolve cwd=$fixture no_build=1 explicit= args=--print
aot --workspace-root $fixture
aot --workspace-root $fixture --check"
actual="$(<"$CALLS_FILE")"
if [[ "$actual" != "$expected" ]]; then
  printf 'package bootstrap call order drifted\nexpected:\n%s\nactual:\n%s\n' \
    "$expected" "$actual" >&2
  exit 1
fi

# A release/local caller's stable binary must remain the execution boundary.
# A complete generated payload is checked without becoming a concurrent writer.
: > "$CALLS_FILE"
stable_harn="$tmp_root/bin/stable-harn"
printf '#!/usr/bin/env bash\nexit 0\n' > "$stable_harn"
chmod +x "$stable_harn"
HARN_BIN="$stable_harn" package_verify_prepare_tools "$fixture"
expected="cargo cwd=$fixture args=build -p harn-cli-aot-gen --bin harn-cli-aot-gen
resolve cwd=$fixture no_build=1 explicit=$stable_harn args=--print
aot --workspace-root $fixture --check"
actual="$(<"$CALLS_FILE")"
if [[ "$actual" != "$expected" ]]; then
  printf 'stable package bootstrap contract drifted\nexpected:\n%s\nactual:\n%s\n' \
    "$expected" "$actual" >&2
  exit 1
fi

workflow="$repo_root/.github/workflows/ci.yml"
if ! grep -Fq -- "- 'scripts/lib/package_verify_bootstrap.sh'" "$workflow"; then
  echo 'package bootstrap changes must route to the package-audit lane' >&2
  exit 1
fi
if grep -Fq 'Generate release/package CLI AOT payload' "$workflow"; then
  echo 'package audit must not restore a separate pre-verifier AOT build' >&2
  exit 1
fi

echo "package_verify_bootstrap_test: ok"
