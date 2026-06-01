#!/usr/bin/env bash
set -euo pipefail

# Measure the in-process embedding surface of `harn-serve` across three
# feature configurations and assert that the lean configurations actually
# shed the heavyweight dependency families (tree-sitter grammars + sqlx).
#
# Background: Burin's Rust TUI links Harn in-process through
# `harn-serve` + `harn-hostlib` + `harn-vm`. Before the feature split, even
# a tiny smoke eval compiled all ~27 tree-sitter grammar crates and the
# full sqlx-postgres tree. This script is the regression guard for issue
# #2781 — it keeps the lean build lean and documents the delta.
#
# Configurations (all on `harn-serve`):
#   - lean        : --no-default-features   (no hostlib, no Postgres)
#   - lean+tools  : --features hostlib       (deterministic tools, no grammars)
#   - full        : --features full          (CLI parity: grammars + Postgres)
#
# Dependency count is used as the comparison metric (cheap, deterministic,
# CI-friendly). Pass `--build` to additionally report cold `cargo build`
# wall time for the lean vs full configs.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PKG="harn-serve"

# Unique normal-dependency crate names for a given feature flag set.
deps_for() {
    cargo tree -p "$PKG" "$@" --edges normal --prefix none 2>/dev/null \
        | grep -v '(\*)' \
        | awk '{print $1}' \
        | grep -E '^[a-z]' \
        | sort -u
}

count() { wc -l | tr -d ' '; }

echo "Resolving dependency sets for $PKG ..."
LEAN="$(deps_for --no-default-features)"
LEAN_TOOLS="$(deps_for --features hostlib)"
FULL="$(deps_for --features full)"

n_lean="$(printf '%s\n' "$LEAN" | count)"
n_lean_tools="$(printf '%s\n' "$LEAN_TOOLS" | count)"
n_full="$(printf '%s\n' "$FULL" | count)"

grammars_in() { printf '%s\n' "$1" | grep -c '^tree-sitter' || true; }
sqlx_in() { printf '%s\n' "$1" | grep -c '^sqlx' || true; }

g_lean="$(grammars_in "$LEAN")"
g_full="$(grammars_in "$FULL")"
s_lean="$(sqlx_in "$LEAN")"
s_full="$(sqlx_in "$FULL")"

printf '\n%-14s %12s %12s %12s\n' "config" "total deps" "grammars" "sqlx"
printf '%-14s %12s %12s %12s\n' "lean" "$n_lean" "$g_lean" "$s_lean"
printf '%-14s %12s %12s %12s\n' "lean+tools" "$n_lean_tools" "$(grammars_in "$LEAN_TOOLS")" "$(sqlx_in "$LEAN_TOOLS")"
printf '%-14s %12s %12s %12s\n' "full" "$n_full" "$g_full" "$s_full"
printf '\nlean build sheds %d crates vs full (%d -> %d)\n' \
    "$((n_full - n_lean))" "$n_full" "$n_lean"

fail=0
assert() {
    local desc="$1" cond="$2"
    if [[ "$cond" == "ok" ]]; then
        echo "PASS: $desc"
    else
        echo "FAIL: $desc"
        fail=1
    fi
}

# The lean configs must not link a single grammar or sqlx crate; the full
# config must link all of them (CLI parity). A regression in either
# direction (an unconditional grammar dep creeping back in, or the full set
# silently shrinking) trips the gate.
assert "lean build links no tree-sitter grammar" "$([[ "$g_lean" -eq 0 ]] && echo ok)"
assert "lean+tools build links no tree-sitter grammar" "$([[ "$(grammars_in "$LEAN_TOOLS")" -eq 0 ]] && echo ok)"
assert "lean build links no sqlx crate" "$([[ "$s_lean" -eq 0 ]] && echo ok)"
assert "full build links the grammar set" "$([[ "$g_full" -ge 27 ]] && echo ok)"
assert "full build links sqlx" "$([[ "$s_full" -ge 1 ]] && echo ok)"
assert "lean is strictly smaller than full" "$([[ "$n_lean" -lt "$n_full" ]] && echo ok)"

if [[ "${1:-}" == "--build" ]]; then
    echo
    echo "Cold build wall time (cargo build, fresh target dir each):"
    for cfg in "--no-default-features" "--features full"; do
        tmp="$(mktemp -d)"
        start=$(date +%s)
        CARGO_TARGET_DIR="$tmp" cargo build -p "$PKG" $cfg >/dev/null 2>&1 || true
        end=$(date +%s)
        printf '  %-22s %4ds\n' "$cfg" "$((end - start))"
        rm -rf "$tmp"
    done
fi

if [[ "$fail" -ne 0 ]]; then
    echo
    echo "Lean embedding surface regressed. See issue #2781." >&2
    exit 1
fi
echo
echo "Lean embedding surface OK."
