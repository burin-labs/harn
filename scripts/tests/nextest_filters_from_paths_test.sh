#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
filter_script="$repo_root/scripts/nextest_filters_from_paths.sh"
: "${HARN_BIN:?run this integration through make test-pr-gate-post-warm-integrations}"
if [[ ! -x "$HARN_BIN" ]]; then
  echo "HARN_BIN is not executable: $HARN_BIN" >&2
  exit 1
fi

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

if [[ -n "$("$filter_script")" ]]; then
  echo "zero paths should produce an empty filter" >&2
  exit 1
fi

fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin"
cat > "$fake_bin/python3" <<'SH'
#!/usr/bin/env bash
echo "fake python should not run" >&2
exit 42
SH
chmod +x "$fake_bin/python3"

expected="binary(orchestrator_http) or package(harn-cli) or package(harn-vm)"
actual=$(PATH="$fake_bin:$PATH" "$filter_script" \
  "crates/harn-cli/tests/orchestrator_http/admin.rs" \
  "crates/harn-cli/tests/test_util/process.rs" \
  "crates/harn-vm/src/flow/slice.rs")
if [[ "$actual" != "$expected" ]]; then
  printf 'expected filter:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
  exit 1
fi
