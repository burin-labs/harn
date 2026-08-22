#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  HARN_NEXTEST_FILTER='<expression>' make test-focused

Optional selectors:
  HARN_NEXTEST_PACKAGE=<crate>
  HARN_NEXTEST_BINARY=<integration-test-binary>  # requires PACKAGE

Runs a focused cargo-nextest expression through an argv boundary. The filter
is never evaluated by Make or a shell parser.
EOF
}

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
filter="${HARN_NEXTEST_FILTER:-}"
package="${HARN_NEXTEST_PACKAGE:-}"
binary="${HARN_NEXTEST_BINARY:-}"
cargo_runner="${HARN_NEXTEST_CARGO_RUNNER:-$repo_root/scripts/cargo_with_worktree_build_dir.sh}"

if [[ -z "$filter" ]]; then
  echo "error: HARN_NEXTEST_FILTER must contain a nextest filter expression" >&2
  usage >&2
  exit 2
fi
if [[ -n "$binary" && -z "$package" ]]; then
  echo "error: HARN_NEXTEST_BINARY requires HARN_NEXTEST_PACKAGE" >&2
  exit 2
fi
if [[ ! -x "$cargo_runner" ]]; then
  echo "error: Cargo runner is not executable: $cargo_runner" >&2
  exit 2
fi

selector=(--workspace)
if [[ -n "$package" ]]; then
  selector=(--package "$package")
fi
if [[ -n "$binary" ]]; then
  selector+=(--test "$binary")
fi

# These variables configure this boundary, not Cargo or the test processes.
# Consume the namespace so strict downstream launchers do not reject current or
# future controls and tests cannot accidentally depend on the wrapper inputs.
while IFS= read -r variable; do
  unset "$variable"
done < <(compgen -A variable HARN_NEXTEST_)

exec "$cargo_runner" nextest run "${selector[@]}" -E "$filter"
