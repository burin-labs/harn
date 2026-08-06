#!/usr/bin/env bash
# nextest_filters_from_paths.sh — compatibility wrapper for the Harn
# cargo-nextest filterset generator.
#
# Usage:
#   ./scripts/nextest_filters_from_paths.sh [file1 file2 ...]
#
# Outputs a nextest -E filter expression on stdout (e.g.
# "binary(orchestrator_http) or package(harn-vm)"), or nothing if no
# Rust test-relevant paths are given.
#
set -euo pipefail

if [[ $# -eq 0 ]]; then
    exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

exec "$repo_root/scripts/harn_bin.sh" run "$repo_root/scripts/nextest_filters_from_paths.harn" -- --root "$repo_root" "$@"
