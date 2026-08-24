#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="${HARN_RELEASE_ROOT:-$(cd "$script_dir/.." && pwd)}"
cd "$root"

harn_bin="${HARN_BIN:-}"
if [[ -z "$harn_bin" || ! -x "$harn_bin" ]]; then
  echo "error: HARN_BIN must name the already-built release-source Harn executable" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
  echo "error: development version preparation requires a clean tree" >&2
  exit 1
fi

metadata="$script_dir/release_metadata.harn"
current="$($harn_bin run "$metadata" -- current --root "$root")"
target="$($harn_bin run "$metadata" -- development-target --root "$root")"

"$harn_bin" run "$metadata" -- develop --root "$root"

# Cargo owns Cargo.lock. The first resolution applies the manifest rewrite;
# the locked resolution proves that the resulting graph is complete.
cargo metadata --format-version=1 >/dev/null
cargo metadata --format-version=1 --locked >/dev/null

"$harn_bin" run "$script_dir/sync_protocol_fixture_runtime_versions.harn" -- \
  --from "$current" --to "$target"
"$harn_bin" dump-protocol-artifacts --artifact-version "$target"
"$harn_bin" run "$script_dir/sync_grammar_fitness_receipt.harn"

actual="$($harn_bin run "$metadata" -- current --root "$root")"
if [[ "$actual" != "$target" ]]; then
  echo "error: expected development version $target, got $actual" >&2
  exit 1
fi
echo "Development version updated: $current -> $target"
