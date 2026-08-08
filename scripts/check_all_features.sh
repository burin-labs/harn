#!/usr/bin/env bash
# Compile every workspace feature set that CI is expected to keep green.
#
# Default lanes intentionally omit off-by-default features (guard-neural /
# testbench-wasi / …). Without a dedicated gate those surfaces rot — as with
# harn-guard's ort `download-binaries` missing a TLS feature (#5690).
#
# Strategy:
#   1. `cargo check --workspace --all-features`, excluding `harn-hostlib`.
#      That package's `computer-local` feature needs desktop capture libraries
#      and is intentionally kept out of headless CI; we check its lean+schema
#      surface (`full,computer`) instead.
#   2. `ORT_SKIP_DOWNLOAD=1` so ort's build script validates the TLS/ureq
#      compile path without fetching ONNX Runtime binaries on every run.
#
# Structural optional-dep feature contracts live in
# `make check-optional-dep-feature-contracts` (required audit-scripts lane).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo_cmd=(./scripts/cargo_with_worktree_build_dir.sh)
if [[ -n "${HARN_CARGO_CMD:-}" ]]; then
  # Allow tests / callers to inject a wrapper (space-separated).
  # shellcheck disable=SC2206
  cargo_cmd=(${HARN_CARGO_CMD})
fi

echo "==> cargo check --workspace --all-features (exclude harn-hostlib)"
ORT_SKIP_DOWNLOAD=1 \
  "${cargo_cmd[@]}" check --locked --workspace --all-features --exclude harn-hostlib

echo "==> cargo check -p harn-hostlib --features full,computer"
ORT_SKIP_DOWNLOAD=1 \
  "${cargo_cmd[@]}" check --locked -p harn-hostlib --features full,computer

echo "all-features compile gate ok"
