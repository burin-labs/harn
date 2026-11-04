#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="${HARN_RELEASE_ROOT:-$(cd "$script_dir/.." && pwd)}"
cd "$root"

harn_bin="${HARN_BIN:-}"
if [[ -z "$harn_bin" || ! -x "$harn_bin" ]]; then
  echo "error: HARN_BIN must name the qualified development paperwork interpreter" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
  echo "error: development version preparation requires a clean tree" >&2
  exit 1
fi

metadata="$script_dir/release_metadata.harn"
current="$($harn_bin run "$metadata" -- current --root "$root")"
# The published release is the authority for what comes next: it may come from any
# older main commit, so the workspace alone cannot say.
released="${RELEASE_PUBLISHED_VERSION:-}"
if [[ -z "$released" ]]; then
  echo "error: RELEASE_PUBLISHED_VERSION must name the published release" >&2
  exit 1
fi
if [[ -n "${RETIRE_SOURCE_SHA:-}" ]]; then
  target="$($harn_bin run "$metadata" -- development-target --root "$root")"
  "$harn_bin" run "$metadata" -- retire --root "$root" --version "$current" --released "$released"
else
  target="$($harn_bin run "$metadata" -- development-target --root "$root" --released "$released")"
  "$harn_bin" run "$metadata" -- develop --root "$root" --released "$released"
  # Record exactly what the release published: the fragments in its tag's tree.
  # Fragments landed on main since then belong to the next release.
  published_fragments="$(mktemp -d)"
  trap 'rm -rf "$published_fragments"' EXIT
  git archive "v${released#v}" changelog.d | tar -x -C "$published_fragments"
  "$harn_bin" run --no-sandbox "$script_dir/release_changelog_fold.harn" -- fold-published \
    --root "$root" --version "${released#v}" --fragments-root "$published_fragments"
fi

# Cargo owns Cargo.lock. The first resolution applies the manifest rewrite;
# the locked resolution proves that the resulting graph is complete.
cargo metadata --format-version=1 >/dev/null
cargo metadata --format-version=1 --locked >/dev/null

"$harn_bin" run "$script_dir/sync_protocol_fixture_runtime_versions.harn" -- \
  --from "$current" --to "$target"
"$harn_bin" dump-protocol-artifacts
"$harn_bin" run "$script_dir/sync_grammar_fitness_receipt.harn"

actual="$($harn_bin run "$metadata" -- current --root "$root")"
if [[ "$actual" != "$target" ]]; then
  echo "error: expected next version $target, got $actual" >&2
  exit 1
fi
echo "Next version declared: $current -> $target"
