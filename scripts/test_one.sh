#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/test_one.sh --package <crate> --lib <fully-qualified-test>
  scripts/test_one.sh --package <crate> --test <binary> <fully-qualified-test>

Runs exactly one Rust test through Cargo without enumerating unrelated test
binaries. A zero-match receipt is an error.
EOF
}

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
package=""
selector=()
test_name=""

while (($# > 0)); do
  case "$1" in
    --package)
      if (($# < 2)) || [[ -z "$2" ]]; then
        echo "error: --package requires a crate name" >&2
        exit 2
      fi
      package="$2"
      shift 2
      ;;
    --lib)
      if ((${#selector[@]} > 0)); then
        echo "error: choose exactly one of --lib or --test" >&2
        exit 2
      fi
      selector=(--lib)
      shift
      ;;
    --test)
      if (($# < 2)) || [[ -z "$2" ]]; then
        echo "error: --test requires a test-binary name" >&2
        exit 2
      fi
      if ((${#selector[@]} > 0)); then
        echo "error: choose exactly one of --lib or --test" >&2
        exit 2
      fi
      selector=(--test "$2")
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    --)
      shift
      ;;
    -*)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
    *)
      if [[ -n "$test_name" ]]; then
        echo "error: expected exactly one fully-qualified test name" >&2
        exit 2
      fi
      test_name="$1"
      shift
      ;;
  esac
done

if [[ -z "$package" || ${#selector[@]} -eq 0 || -z "$test_name" ]]; then
  echo "error: --package, one target selector, and a test name are required" >&2
  usage >&2
  exit 2
fi

cargo_runner="${HARN_TEST_ONE_CARGO_RUNNER:-$repo_root/scripts/cargo_with_worktree_build_dir.sh}"
if [[ ! -x "$cargo_runner" ]]; then
  echo "error: Cargo runner is not executable: $cargo_runner" >&2
  exit 2
fi

receipt=$(mktemp "${TMPDIR:-/tmp}/harn-test-one.XXXXXX")
trap 'rm -f "$receipt"' EXIT

set +e
"$cargo_runner" test --package "$package" "${selector[@]}" "$test_name" -- \
  --exact --format terse 2>&1 | tee "$receipt"
pipeline_status=("${PIPESTATUS[@]}")
set -e

if [[ ${pipeline_status[1]} -ne 0 ]]; then
  echo "error: failed to record the exact-test receipt" >&2
  exit "${pipeline_status[1]}"
fi
if [[ ${pipeline_status[0]} -ne 0 ]]; then
  exit "${pipeline_status[0]}"
fi
if ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;' "$receipt"; then
  echo "error: exact test did not produce a one-test success receipt: $test_name" >&2
  exit 1
fi
