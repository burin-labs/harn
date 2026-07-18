#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# shellcheck source=/dev/null
source "$root/.githooks/lib.sh"

record="$tmp/selection"
hook_export_harn_bin() {
  printf 'reuse\n' >> "$record"
  HARN_BIN="$tmp/fake-harn"
  export HARN_BIN
}
hook_export_fresh_worktree_harn_bin() {
  printf 'fresh\n' >> "$record"
  HARN_BIN="$tmp/fake-harn"
  export HARN_BIN
}

cat > "$tmp/fake-harn" <<'SH'
#!/bin/sh
printf 'run:%s\n' "$*" >> "$HOOK_REGISTRY_RECORD"
SH
chmod +x "$tmp/fake-harn"
export HOOK_REGISTRY_RECORD="$record"

printf '%s\n' '.github/workflows/ci.yml' > "$tmp/non-rust"
hook_check_generated_registry "$tmp/non-rust"
if [[ "$(cat "$record")" != $'reuse\nrun:run scripts/check_generated_registry.harn' ]]; then
  echo "non-Rust registry change did not reuse the authorized Harn binary" >&2
  exit 1
fi

: > "$record"
printf '%s\n' '.github/workflows/ci.yml' 'crates/harn-vm/src/lib.rs' > "$tmp/rust"
hook_check_generated_registry "$tmp/rust"
if [[ "$(cat "$record")" != $'fresh\nrun:run scripts/check_generated_registry.harn' ]]; then
  echo "Rust registry change did not select a fresh worktree Harn binary" >&2
  exit 1
fi

# Crates compile non-Rust assets in via include_str!, so editing one changes
# what the binary emits even though no .rs file moved. Reusing a stale binary
# here makes a drift gate compare generated output against the old inputs and
# pass — the exact way an un-regenerated capability table or diagnostic
# explanation ships unnoticed.
for asset in \
  'crates/harn-parser/src/diagnostic_codes/explanations/HARN-OWN-003.md' \
  'crates/harn-vm/src/llm/capabilities.toml' \
  'crates/harn-stdlib/src/stdlib/cli/scaffold/init.harn' \
  'tree-sitter-harn/src/node-types.json'; do
  : > "$record"
  printf '%s\n' "$asset" > "$tmp/asset"
  hook_check_generated_registry "$tmp/asset"
  if [[ "$(cat "$record")" != $'fresh\nrun:run scripts/check_generated_registry.harn' ]]; then
    echo "compiled-in asset $asset did not select a fresh worktree Harn binary" >&2
    exit 1
  fi
done

# A path outside the crates that cannot reach the binary still reuses it, so
# the broadened pattern does not turn every docs edit into a rebuild.
: > "$record"
printf '%s\n' 'docs/src/introduction.md' 'README.md' > "$tmp/docs-only"
hook_check_generated_registry "$tmp/docs-only"
if [[ "$(cat "$record")" != $'reuse\nrun:run scripts/check_generated_registry.harn' ]]; then
  echo "docs-only change should reuse the authorized Harn binary" >&2
  exit 1
fi

for hook in .githooks/pre-commit .githooks/pre-push; do
  helper_count="$(grep -c "hook_check_generated_registry \"\$changed\"" "$root/$hook")"
  direct_count="$(grep -c 'hook_export_fresh_worktree_harn_bin' "$root/$hook")"
  if [[ "$helper_count" -ne 1 || "$direct_count" -ne 1 ]]; then
    echo "$hook must use one registry selector and reserve one direct fresh build for generated mirrors" >&2
    exit 1
  fi
done

echo "hook registry Harn binary selection OK"
