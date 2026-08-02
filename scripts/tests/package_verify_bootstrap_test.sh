#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
# shellcheck source=scripts/lib/package_verify_bootstrap.sh
source "$repo_root/scripts/lib/package_verify_bootstrap.sh"

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/repo"
mkdir -p "$fixture/scripts" "$fixture/crates/harn-cli/generated" "$tmp_root/bin"

fake_harn="$tmp_root/bin/harn"
printf '#!/usr/bin/env bash\nexit 0\n' > "$fake_harn"
chmod +x "$fake_harn"

cat > "$fixture/scripts/harn_bin.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'resolve %s\n' "$*" >> "$CALLS_FILE"
printf '%s\n' "$FAKE_HARN"
SH
chmod +x "$fixture/scripts/harn_bin.sh"

cat > "$tmp_root/bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'make %s\n' "$*" >> "$CALLS_FILE"
if [[ " $* " == *" gen-cli-aot "* ]]; then
  mkdir -p "$FIXTURE_ROOT/crates/harn-cli/generated"
  printf '{}\n' > "$FIXTURE_ROOT/crates/harn-cli/generated/cli-bytecode-manifest.json"
fi
SH
chmod +x "$tmp_root/bin/make"

export CALLS_FILE="$tmp_root/calls"
export FAKE_HARN="$fake_harn"
export FIXTURE_ROOT="$fixture"
export PATH="$tmp_root/bin:$PATH"

package_verify_resolve_harn_bin "$fixture"
[[ "$HARN_BIN" == "$fake_harn" ]]
package_verify_ensure_cli_aot "$fixture"
package_verify_ensure_cli_aot "$fixture"

expected="resolve --print
make --no-print-directory -C $fixture gen-cli-aot
make --no-print-directory -C $fixture check-cli-aot
make --no-print-directory -C $fixture check-cli-aot"
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
