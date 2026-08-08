#!/usr/bin/env bash
set -euo pipefail

# Compatibility launcher for the Harn implementation. Keep orchestration in
# Harn while preserving existing callers of scripts/gha_spend_report.sh.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -n "${HARN_BIN:-}" ]]; then
  exec "${HARN_BIN}" run "${repo_root}/scripts/gha_spend_report.harn" -- "$@"
fi
exec cargo run --quiet --bin harn -- run "${repo_root}/scripts/gha_spend_report.harn" -- "$@"
