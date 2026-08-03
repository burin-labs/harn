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

cp "$repo_root/scripts/lib/harn_bin.sh" "$fixture/scripts/lib/harn_bin.sh"

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
if [[ "${FAKE_CARGO_WINDOWS:-0}" == "1" ]]; then
  cp "$AOT_GENERATOR_TEMPLATE" \
    "$FIXTURE_ROOT/target/debug/harn-cli-aot-gen.exe"
  chmod +x "$FIXTURE_ROOT/target/debug/harn-cli-aot-gen.exe"
fi
SH
chmod +x "$fixture/scripts/cargo_with_worktree_build_dir.sh"

cat > "$fixture/target/debug/harn-cli-aot-gen" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
# Every case must reach the generator through a snapshot the caller owns. A
# concurrent Cargo build relinking the shared target copy killed this process
# mid-run during release audits (#6102), so refuse to run from there at all.
case "$0" in
  "$FIXTURE_ROOT"/target/*)
    printf 'aot generator ran from the shared target dir: %s\n' "$0" >&2
    exit 1
    ;;
esac
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

# Stands in for the trapped scratch directory verify_crate_packages.sh owns.
tool_dir="$tmp_root/aot-tools"

# This case proves the cold bootstrap contract. Keep it independent from the
# aggregate gate's deliberately warmed Harn execution boundary; explicit-bin
# behavior has its own case below.
unset HARN_BIN HARN_BIN_NO_BUILD
package_verify_prepare_tools "$fixture" "$tool_dir"
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
HARN_BIN="$stable_harn" package_verify_prepare_tools "$fixture" "$tool_dir"
expected="cargo cwd=$fixture args=build -p harn-cli-aot-gen --bin harn-cli-aot-gen
resolve cwd=$fixture no_build=1 explicit=$stable_harn args=--print
aot --workspace-root $fixture --check"
actual="$(<"$CALLS_FILE")"
if [[ "$actual" != "$expected" ]]; then
  printf 'stable package bootstrap contract drifted\nexpected:\n%s\nactual:\n%s\n' \
    "$expected" "$actual" >&2
  exit 1
fi

# The documented no-build boundary covers every executable this helper needs,
# not only Harn itself.
: > "$CALLS_FILE"
HARN_BIN="$stable_harn" HARN_BIN_NO_BUILD=1 \
  package_verify_prepare_tools "$fixture" "$tool_dir"
expected="resolve cwd=$fixture no_build=1 explicit=$stable_harn args=--print
aot --workspace-root $fixture --check"
actual="$(<"$CALLS_FILE")"
if [[ "$actual" != "$expected" ]]; then
  printf 'no-build package bootstrap contract drifted\nexpected:\n%s\nactual:\n%s\n' \
    "$expected" "$actual" >&2
  exit 1
fi

mv "$fixture/target/debug/harn-cli-aot-gen" \
  "$fixture/target/debug/harn-cli-aot-gen.not-built"
: > "$CALLS_FILE"
if HARN_BIN="$stable_harn" HARN_BIN_NO_BUILD=1 \
  package_verify_prepare_tools "$fixture" "$tool_dir" 2>"$tmp_root/no-build.err"; then
  echo 'no-build package bootstrap accepted a missing AOT generator' >&2
  exit 1
fi
if [[ -s "$CALLS_FILE" ]]; then
  echo 'no-build package bootstrap invoked a tool before failing' >&2
  exit 1
fi
if ! grep -Fq 'HARN_BIN_NO_BUILD=1' "$tmp_root/no-build.err"; then
  echo 'missing AOT generator error did not explain the no-build boundary' >&2
  exit 1
fi
mv "$fixture/target/debug/harn-cli-aot-gen.not-built" \
  "$fixture/target/debug/harn-cli-aot-gen"

# Platform suffix policy must select the artifact a cold Windows Cargo build
# is about to produce, without probing for a not-yet-existing executable.
: > "$CALLS_FILE"
export AOT_GENERATOR_TEMPLATE="$fixture/target/debug/harn-cli-aot-gen"
mv "$fixture/target/debug/harn-cli-aot-gen" "$AOT_GENERATOR_TEMPLATE.template"
export AOT_GENERATOR_TEMPLATE="$AOT_GENERATOR_TEMPLATE.template"
unset HARN_BIN HARN_BIN_NO_BUILD
OS=Windows_NT FAKE_CARGO_WINDOWS=1 package_verify_prepare_tools "$fixture" "$tool_dir"
expected="cargo cwd=$fixture args=build -p harn-cli --bin harn -p harn-cli-aot-gen --bin harn-cli-aot-gen
resolve cwd=$fixture no_build=1 explicit= args=--print
aot --workspace-root $fixture --check"
actual="$(<"$CALLS_FILE")"
if [[ "$actual" != "$expected" ]]; then
  printf 'cold Windows package bootstrap contract drifted\nexpected:\n%s\nactual:\n%s\n' \
    "$expected" "$actual" >&2
  exit 1
fi
rm "$fixture/target/debug/harn-cli-aot-gen.exe"
mv "$AOT_GENERATOR_TEMPLATE" "$fixture/target/debug/harn-cli-aot-gen"

workflow="$repo_root/.github/workflows/ci.yml"
if ! grep -Fq -- "- 'scripts/lib/package_verify_bootstrap.sh'" "$workflow"; then
  echo 'package bootstrap changes must route to the package-audit lane' >&2
  exit 1
fi
if grep -Fq 'Generate release/package CLI AOT payload' "$workflow"; then
  echo 'package audit must not restore a separate pre-verifier AOT build' >&2
  exit 1
fi
package_audit_job="$(awk '
  /^  package-audit:$/ { in_job = 1 }
  in_job && /^  [[:alnum:]_-]+:$/ && $0 != "  package-audit:" { exit }
  in_job { print }
' "$workflow")"
if ! grep -Fq 'HARN_BIN: ""' <<<"$package_audit_job"; then
  echo 'cold package-audit CI must keep the combined Harn/generator build path' >&2
  exit 1
fi

echo "package_verify_bootstrap_test: ok"
