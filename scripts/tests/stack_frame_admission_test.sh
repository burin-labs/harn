#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/worktree"
mkdir -p "$fixture/scripts/ci" "$fixture/scripts/lib" "$tmp_root/bin"
cp "$repo_root/scripts/ci/collect_stack_frames.sh" "$fixture/scripts/ci/"
cp "$repo_root/scripts/lib/cargo_env.sh" "$fixture/scripts/lib/"
printf 'too-many-arguments-threshold = 8\n' > "$fixture/clippy.toml"
export STACK_CENSUS_TARGET="$tmp_root/configured target"
export STACK_CENSUS_CALLS="$tmp_root/calls"
cat > "$tmp_root/bin/cargo" <<'SH'
#!/bin/bash
if [[ "${1:-}" == metadata ]]; then
  printf '{"target_directory":"%s"}\n' "$STACK_CENSUS_TARGET"
  exit 0
fi
echo 'stack measurement bypassed the Cargo admission wrapper' >&2
exit 95
SH
cat > "$fixture/scripts/cargo_with_worktree_build_dir.sh" <<'SH'
#!/bin/bash
set -euo pipefail
[[ "$*" == 'clippy --workspace --all-targets --message-format=json' ]]
[[ "$CARGO_TARGET_DIR" == "$STACK_CENSUS_TARGET/stack-frame-census" ]]
[[ -z "${CARGO_BUILD_BUILD_DIR+x}" ]]
[[ "${RUSTFLAGS-unset}" == '' ]]
grep -Fxq 'stack-size-threshold = 16384' "$CLIPPY_CONF_DIR/clippy.toml"
grep -Fxq 'too-many-arguments-threshold = 8' "$CLIPPY_CONF_DIR/clippy.toml"
printf 'admitted\n' >> "$STACK_CENSUS_CALLS"
if [[ "${STACK_CENSUS_EMPTY:-0}" == 0 ]]; then
  printf '{"message":{"code":{"code":"clippy::large_stack_frames"}}}\n'
fi
SH
chmod +x "$tmp_root/bin/cargo" "$fixture/scripts/cargo_with_worktree_build_dir.sh"
PATH="$tmp_root/bin:$PATH" CARGO_BUILD_BUILD_DIR="$tmp_root/wrong-target" \
  RUSTFLAGS='-D warnings' env -u CARGO_TARGET_DIR \
  "$fixture/scripts/ci/collect_stack_frames.sh" "$tmp_root/raw.json"
[[ $(wc -l < "$STACK_CENSUS_CALLS") -eq 1 ]]
if PATH="$tmp_root/bin:$PATH" STACK_CENSUS_EMPTY=1 env -u CARGO_TARGET_DIR \
  "$fixture/scripts/ci/collect_stack_frames.sh" "$tmp_root/empty.json" \
  > "$tmp_root/empty.out" 2>&1; then
  echo 'empty stack census unexpectedly passed' >&2
  exit 1
fi
grep -Fq 'the census measured nothing' "$tmp_root/empty.out"
echo 'stack-frame admission tests passed'
