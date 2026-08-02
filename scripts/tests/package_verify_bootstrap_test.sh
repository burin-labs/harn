#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
# shellcheck source=scripts/lib/package_verify_bootstrap.sh
source "$repo_root/scripts/lib/package_verify_bootstrap.sh"

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/repo"
mkdir -p "$fixture/scripts" "$fixture/target/debug" "$tmp_root/bin"

fake_harn="$fixture/target/debug/harn"
printf '#!/usr/bin/env bash\nexit 0\n' > "$fake_harn"
chmod +x "$fake_harn"

cat > "$fixture/scripts/harn_bin.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'resolve %s\n' "$*" >> "$CALLS_FILE"
printf '%s\n' "$FAKE_HARN"
SH
chmod +x "$fixture/scripts/harn_bin.sh"

cat > "$fixture/scripts/cargo_with_worktree_build_dir.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo %s\n' "$*" >> "$CALLS_FILE"
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

package_verify_prepare_tools "$fixture"
[[ "$HARN_BIN" == "$fake_harn" ]]

expected="cargo build -p harn-cli --bin harn -p harn-cli-aot-gen --bin harn-cli-aot-gen
resolve --print
aot --workspace-root $fixture
aot --workspace-root $fixture --check"
actual="$(<"$CALLS_FILE")"
if [[ "$actual" != "$expected" ]]; then
  printf 'package bootstrap call order drifted\nexpected:\n%s\nactual:\n%s\n' \
    "$expected" "$actual" >&2
  exit 1
fi

workflow="$repo_root/.github/workflows/ci.yml"
grep -Fq -- "- 'scripts/lib/package_verify_bootstrap.sh'" "$workflow"
if grep -Fq 'Generate release/package CLI AOT payload' "$workflow"; then
  echo 'package audit must not restore a separate pre-verifier AOT build' >&2
  exit 1
fi

echo "package_verify_bootstrap_test: ok"
