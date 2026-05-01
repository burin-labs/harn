#!/usr/bin/env bash
# nextest_filters_from_paths.sh — translate touched file paths into a
# cargo-nextest filterset expression.
#
# Usage:
#   ./scripts/nextest_filters_from_paths.sh [file1 file2 ...]
#
# Outputs a nextest -E filter expression on stdout (e.g.
# "binary(orchestrator_http) or package(harn-vm)"), or nothing if no
# Rust test-relevant paths are given.  Exit code is always 0.
#
# Mapping rules (first match wins):
#
#   crates/<pkg>/tests/<name>.rs          → binary(<name>)
#     Top-level integration test file = its own nextest binary.
#
#   crates/<pkg>/tests/<dir>/<file>.rs    → binary(<dir>)   (when <dir>.rs exists)
#                                         → package(<pkg>)  (shared support dir)
#     Subdirectory files are either modules of the same-named binary
#     (e.g. orchestrator_http/a2a.rs lives under orchestrator_http.rs)
#     or shared helpers (e.g. support/, test_util/) — check for the
#     sibling .rs entry to tell them apart.
#
#   crates/<pkg>/...                      → package(<pkg>)
#     Unit tests in src/ or any other crate-local change.
#
# Designed to be called from the flake-detection CI workflow.

set -euo pipefail

if [[ $# -eq 0 ]]; then
    exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

declare -A seen
filters=()

add_filter() {
    local f="$1"
    if [[ -z "${seen[$f]+_}" ]]; then
        seen["$f"]=1
        filters+=("$f")
    fi
}

for path in "$@"; do
    # Strip leading ./ and skip blank entries.
    path="${path#./}"
    [[ -z "$path" ]] && continue

    # crates/<pkg>/tests/<name>.rs — top-level integration test binary.
    if [[ "$path" =~ ^crates/([^/]+)/tests/([^/]+)\.rs$ ]]; then
        add_filter "binary(${BASH_REMATCH[2]})"
        continue
    fi

    # crates/<pkg>/tests/<dir>/... — module file or shared support directory.
    if [[ "$path" =~ ^crates/([^/]+)/tests/([^/]+)/ ]]; then
        pkg="${BASH_REMATCH[1]}"
        dir="${BASH_REMATCH[2]}"
        if [[ -f "${repo_root}/crates/${pkg}/tests/${dir}.rs" ]]; then
            add_filter "binary(${dir})"
        else
            add_filter "package(${pkg})"
        fi
        continue
    fi

    # crates/<pkg>/... — unit tests in src/ or any other package file.
    if [[ "$path" =~ ^crates/([^/]+)/ ]]; then
        add_filter "package(${BASH_REMATCH[1]})"
        continue
    fi
done

if [[ ${#filters[@]} -eq 0 ]]; then
    exit 0
fi

result=""
for f in "${filters[@]}"; do
    if [[ -n "$result" ]]; then
        result="$result or $f"
    else
        result="$f"
    fi
done

printf '%s\n' "$result"
