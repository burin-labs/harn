#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/test_one.sh --package <crate> --lib <fully-qualified-test>
  scripts/test_one.sh --package <crate> --test <binary> <fully-qualified-test>

Runs exactly one Rust test through Cargo without enumerating unrelated test
binaries.

A test name is only reachable through the target kind that defines it: names
under `src/` belong to the library target, names under `tests/` belong to an
integration-test binary. Pointing one kind's selector at the other kind's name
produces a filter that cannot match, so this runner resolves the requested
target from the package manifest and asks that target to list the name before
running anything. An unservable request is refused up front, and a zero-match
receipt from a servable one is still an error.
EOF
}

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
package=""
selector=()
target_kind=""
target_name=""
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
      target_kind="lib"
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
      target_kind="test"
      target_name="$2"
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
# `HARN_TEST_ONE_*` belongs to this shell boundary. Leaving any current or
# future control exported makes the strict Harn CLI reject it as an unknown
# runtime variable when the Cargo wrapper probes a lease runner. Consume the
# namespace before invoking either Harn or Cargo.
while IFS= read -r variable; do
  unset "$variable"
done < <(compgen -A variable HARN_TEST_ONE_)
if [[ ! -x "$cargo_runner" ]]; then
  echo "error: Cargo runner is not executable: $cargo_runner" >&2
  exit 2
fi

metadata=$(mktemp "${TMPDIR:-/tmp}/harn-test-one-metadata.XXXXXX")
listing=$(mktemp "${TMPDIR:-/tmp}/harn-test-one-listing.XXXXXX")
receipt=$(mktemp "${TMPDIR:-/tmp}/harn-test-one.XXXXXX")
trap 'rm -f "$metadata" "$listing" "$receipt"' EXIT

if [[ "$target_kind" == "lib" ]]; then
  requested_target="library target"
else
  requested_target="integration-test binary '$target_name'"
fi

# A Rust test name is reachable only through the target that compiled it, and a
# selector aimed at the wrong kind filters to nothing rather than failing. Both
# checks below exist so that shape is refused with a cause instead of running an
# empty filter and leaving the receipt check to infer one.
#
# The first check reads the package's declared targets. That is manifest
# metadata, not a build, so a request naming a target the package does not have
# costs nothing to reject.
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required to resolve the requested Cargo target" >&2
  exit 2
fi
if ! "$cargo_runner" metadata --no-deps --format-version 1 > "$metadata"; then
  echo "error: could not read workspace metadata for package $package" >&2
  exit 2
fi
inventory=$(jq -r --arg package "$package" '
  .packages[]
  | select(.name == $package)
  | .targets[]
  | .kind[] as $kind
  | [$kind, .name]
  | @tsv
' "$metadata" | sort -u)
if [[ -z "$inventory" ]]; then
  echo "error: no package named '$package' in this workspace" >&2
  exit 2
fi

describe_targets() {
  local kind name
  while IFS=$'\t' read -r kind name; do
    [[ -n "$kind" ]] || continue
    printf '  %s target: %s\n' "$kind" "$name"
  done <<< "$inventory"
}

has_target() {
  local want_kind="$1" want_name="$2" kind name
  while IFS=$'\t' read -r kind name; do
    if [[ "$kind" == "$want_kind" ]] && [[ -z "$want_name" || "$name" == "$want_name" ]]; then
      return 0
    fi
  done <<< "$inventory"
  return 1
}

if ! has_target "$target_kind" "$target_name"; then
  {
    echo "error: package $package has no $requested_target"
    echo "it declares:"
    describe_targets
  } >&2
  exit 2
fi

# The second check asks the resolved target to name the test. Listing is the
# same selector the run uses, so a name it cannot produce is a name the run
# cannot execute — the mismatch surfaces here, with both sides of it named,
# rather than as a silent zero-match filter.
set +e
"$cargo_runner" test --package "$package" "${selector[@]}" "$test_name" -- \
  --exact --list --format terse > "$listing"
listing_status=$?
set -e
if ((listing_status != 0)); then
  echo "error: could not enumerate tests in the $requested_target of package $package" >&2
  exit "$listing_status"
fi
if ! grep -Fqx -- "$test_name: test" "$listing"; then
  {
    echo "error: the $requested_target of package $package defines no test named:"
    echo "  $test_name"
    echo "package $package declares:"
    describe_targets
    if [[ "$target_kind" == "lib" ]]; then
      echo "a name defined under the package's tests/ directory belongs to an"
      echo "integration-test binary; select it with --test <binary> instead of --lib."
    else
      echo "a name defined under the package's src/ directory belongs to the"
      echo "library target; select it with --lib instead of --test $target_name."
    fi
  } >&2
  exit 2
fi

echo "test_one: running $test_name in the $requested_target of package $package" >&2

if ! exec 3> "$receipt"; then
  echo "error: failed to initialize the exact-test receipt" >&2
  exit 1
fi

# Capture through a regular file rather than a live `runner | tee` pipe. A
# runner-side supervisor can outlive Cargo while retaining stdout; on a pipe,
# that unrelated descendant owns EOF and can hold this command open after the
# selected test has completed. A regular file makes the runner process the sole
# lifetime boundary while preserving its status independently from output
# replay.
set +e
"$cargo_runner" test --package "$package" "${selector[@]}" "$test_name" -- \
  --exact --format terse >&3 2>&1
runner_status=$?
set -e
exec 3>&-

if ! cat "$receipt"; then
  echo "error: failed to replay the exact-test receipt" >&2
  exit 1
fi
if ((runner_status != 0)); then
  echo "error: exact-test runner failed with status $runner_status" >&2
  exit "$runner_status"
fi
if ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;' "$receipt"; then
  echo "error: exact test did not produce a one-test success receipt: $test_name" >&2
  exit 1
fi
