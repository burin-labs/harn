#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
filter_script="$repo_root/scripts/nextest_filters_from_paths.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

assert_filter() {
  local expected="$1"
  shift

  local actual
  actual=$("$filter_script" "$@")
  if [[ "$actual" != "$expected" ]]; then
    printf 'expected filter:\n%s\nactual:\n%s\npaths:\n' "$expected" "$actual" >&2
    printf '  %s\n' "$@" >&2
    exit 1
  fi
}

assert_empty_filter() {
  local actual
  actual=$("$filter_script" "$@")
  if [[ -n "$actual" ]]; then
    printf 'expected empty filter, got:\n%s\npaths:\n' "$actual" >&2
    printf '  %s\n' "$@" >&2
    exit 1
  fi
}

assert_filter \
  "binary(orchestrator_http)" \
  "crates/harn-cli/tests/orchestrator_http/admin.rs"

assert_filter \
  "package(harn-cli)" \
  "crates/harn-cli/tests/test_util/process.rs"

assert_filter \
  "package(harn-vm)" \
  "crates/harn-vm/src/flow/slice.rs"

assert_filter \
  "package(harn-vm)" \
  "crates/harn-vm/src/flow/slice.rs" \
  "./crates/harn-vm/src/lib.rs"

assert_filter \
  "binary(orchestrator_http) or package(harn-cli) or package(harn-vm)" \
  "crates/harn-cli/tests/orchestrator_http/admin.rs" \
  "crates/harn-cli/tests/test_util/process.rs" \
  "crates/harn-vm/src/flow/slice.rs"

assert_empty_filter \
  "README.md" \
  "crates/harn-cli/tests/dispatch_fixtures/README.md" \
  ""

fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin"
cat > "$fake_bin/python3" <<'SH'
#!/usr/bin/env bash
echo "fake python should not run" >&2
exit 42
SH
chmod +x "$fake_bin/python3"

PATH="$fake_bin:$PATH" \
  assert_filter \
  "package(harn-vm)" \
  "crates/harn-vm/src/flow/slice.rs"
