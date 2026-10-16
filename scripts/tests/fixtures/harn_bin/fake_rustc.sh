#!/usr/bin/env bash
set -euo pipefail

version="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "${FAKE_RUST_TOOLCHAIN_FILE:?}")"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || exit 1
printf 'rustc %s (fixture)\n' "$version"
