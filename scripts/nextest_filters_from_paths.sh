#!/usr/bin/env bash
# nextest_filters_from_paths.sh — translate touched file paths into a
# cargo-nextest filterset expression.
#
# Usage:
#   ./scripts/nextest_filters_from_paths.sh [file1 file2 ...]
#
# Outputs a nextest -E filter expression on stdout (e.g.
# "binary(orchestrator_http) or package(harn-vm)"), or nothing if no
# Rust test-relevant paths are given.
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
#   crates/<pkg>/...                      → package(<workspace package name>)
#     Unit tests in src/ or any other crate-local change.
#
#   crates/<pkg>/... for workspace-excluded crates is ignored. Those
#     packages are not discoverable by `cargo nextest run --workspace`,
#     so including them would make nextest reject the whole filterset.
#
# Designed to be called from the flake-detection CI workflow.

set -euo pipefail

if [[ $# -eq 0 ]]; then
    exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

workspace_members=()
workspace_package_names=()
seen_filters=()
filters=()

load_workspace_packages() {
    if ! command -v python3 >/dev/null 2>&1; then
        echo "python3 is required to discover Cargo workspace packages" >&2
        return 1
    fi

    local package_map
    package_map=$(mktemp "${TMPDIR:-/tmp}/harn-nextest-filter-packages.XXXXXX")
    if ! python3 - "$repo_root" > "$package_map" <<'PY'
import fnmatch
import pathlib
import sys

try:
    import tomllib
except ModuleNotFoundError:
    print("tomllib is required to read Cargo.toml", file=sys.stderr)
    sys.exit(1)

root = pathlib.Path(sys.argv[1])
with (root / "Cargo.toml").open("rb") as handle:
    workspace = tomllib.load(handle).get("workspace", {})

members = workspace.get("members", [])
excludes = workspace.get("exclude", [])

def is_excluded(relative_path: str) -> bool:
    return any(fnmatch.fnmatch(relative_path, pattern) for pattern in excludes)

for pattern in members:
    matches = sorted(root.glob(pattern))
    if not matches and "*" not in pattern:
        matches = [root / pattern]
    for member_path in matches:
        manifest = member_path / "Cargo.toml"
        if not manifest.is_file():
            continue
        relative = member_path.relative_to(root).as_posix()
        if is_excluded(relative):
            continue
        with manifest.open("rb") as handle:
            package = tomllib.load(handle).get("package", {})
        name = package.get("name")
        if name:
            print(f"{relative}\t{name}")
PY
    then
        rm -f "$package_map"
        echo "failed to discover Cargo workspace packages" >&2
        return 1
    fi

    while IFS=$'\t' read -r member package; do
        [[ -z "$member" || -z "$package" ]] && continue
        workspace_members+=("$member")
        workspace_package_names+=("$package")
    done < "$package_map"
    rm -f "$package_map"
}

workspace_package_for_crate_dir() {
    local crate_dir="$1"
    local index
    for ((index = 0; index < ${#workspace_members[@]}; index++)); do
        if [[ "${workspace_members[$index]}" == "$crate_dir" ]]; then
            printf '%s' "${workspace_package_names[$index]}"
            return 0
        fi
    done
}

add_filter() {
    local f="$1"
    local seen_filter
    for seen_filter in "${seen_filters[@]}"; do
        [[ "$seen_filter" == "$f" ]] && return 0
    done
    seen_filters+=("$f")
    filters+=("$f")
}

load_workspace_packages

for path in "$@"; do
    # Strip leading ./ and skip blank entries.
    path="${path#./}"
    [[ -z "$path" ]] && continue
    [[ "$path" != *.rs ]] && continue

    # crates/<pkg>/tests/<name>.rs — top-level integration test binary.
    if [[ "$path" =~ ^crates/([^/]+)/tests/([^/]+)\.rs$ ]]; then
        pkg="$(workspace_package_for_crate_dir "crates/${BASH_REMATCH[1]}")"
        [[ -z "$pkg" ]] && continue
        add_filter "binary(${BASH_REMATCH[2]})"
        continue
    fi

    # crates/<pkg>/tests/<dir>/... — module file or shared support directory.
    if [[ "$path" =~ ^crates/([^/]+)/tests/([^/]+)/ ]]; then
        crate_dir="${BASH_REMATCH[1]}"
        dir="${BASH_REMATCH[2]}"
        pkg="$(workspace_package_for_crate_dir "crates/${crate_dir}")"
        [[ -z "$pkg" ]] && continue
        if [[ -f "${repo_root}/crates/${crate_dir}/tests/${dir}.rs" ]]; then
            add_filter "binary(${dir})"
        else
            add_filter "package(${pkg})"
        fi
        continue
    fi

    # crates/<pkg>/... — unit tests in src/ or any other package file.
    if [[ "$path" =~ ^crates/([^/]+)/ ]]; then
        pkg="$(workspace_package_for_crate_dir "crates/${BASH_REMATCH[1]}")"
        [[ -z "$pkg" ]] && continue
        add_filter "package(${pkg})"
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
