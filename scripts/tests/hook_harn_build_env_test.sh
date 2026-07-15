#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
target_dir="$tmp_root/target dir"
record="$tmp_root/cargo-record.txt"
mkdir -p "$fake_bin" "$target_dir/debug"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'args=%s\n' "$*"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-__unset__}"
  printf 'CARGO_BUILD_BUILD_DIR=%s\n' "${CARGO_BUILD_BUILD_DIR-__unset__}"
  printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-__unset__}"
  printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-__unset__}"
} >> "$FAKE_CARGO_RECORD"
case "$*" in
  "build --quiet --bin harn")
    mkdir -p "${CARGO_TARGET_DIR:?}/debug"
    cat > "$CARGO_TARGET_DIR/debug/harn" <<'BIN'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_HARN_RECORD"
BIN
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    ;;
  "metadata --format-version=1 --no-deps")
    python3 - <<'PY'
import json
import os
print(json.dumps({"target_directory": os.environ["CARGO_TARGET_DIR"]}))
PY
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_bin/cargo"

hook_repo="$tmp_root/hook-repo"
mkdir -p "$hook_repo/.githooks" "$hook_repo/scripts/lib"
cp "$repo_root/.githooks/lib.sh" "$hook_repo/.githooks/lib.sh"
cp "$repo_root/scripts/lib/cargo_env.sh" "$hook_repo/scripts/lib/cargo_env.sh"
git -C "$hook_repo" init --quiet

(
  cd "$hook_repo"
  # shellcheck source=/dev/null
  . ./.githooks/lib.sh
  RUSTC_WRAPPER=sccache \
    CARGO_BUILD_RUSTC_WRAPPER=sccache \
    CARGO_TARGET_DIR="$target_dir" \
    FAKE_CARGO_RECORD="$record" \
    PATH="$fake_bin:$PATH" \
    hook_ensure_harn > "$tmp_root/hook-harn-path.txt"
)

if [[ "$(cat "$tmp_root/hook-harn-path.txt")" != "$target_dir/debug/harn" ]]; then
  echo "hook_ensure_harn did not preserve CARGO_TARGET_DIR" >&2
  cat "$tmp_root/hook-harn-path.txt" >&2
  exit 1
fi

if ! grep -Fxq "args=build --quiet --bin harn" "$record"; then
  echo "hook_ensure_harn did not build the harn binary" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_TARGET_DIR=$target_dir" "$record"; then
  echo "hook_ensure_harn did not pass through CARGO_TARGET_DIR" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$target_dir" "$record"; then
  echo "hook_ensure_harn did not reuse CARGO_TARGET_DIR for Cargo intermediates" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "RUSTC_WRAPPER=" "$record"; then
  echo "hook_ensure_harn did not clear RUSTC_WRAPPER" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_RUSTC_WRAPPER=" "$record"; then
  echo "hook_ensure_harn did not clear CARGO_BUILD_RUSTC_WRAPPER" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
stale_harn_bin="$tmp_root/stale-harn"
touch "$stale_harn_bin"
(
  cd "$hook_repo"
  # shellcheck source=/dev/null
  . ./.githooks/lib.sh
  export HARN_BIN="$stale_harn_bin"
  RUSTC_WRAPPER=sccache \
    CARGO_BUILD_RUSTC_WRAPPER=sccache \
    CARGO_TARGET_DIR="$target_dir" \
    FAKE_CARGO_RECORD="$record" \
    PATH="$fake_bin:$PATH" \
    hook_export_fresh_worktree_harn_bin
  printf '%s\n' "$HARN_BIN" > "$tmp_root/fresh-hook-harn-path.txt"
)

if [[ "$(cat "$tmp_root/fresh-hook-harn-path.txt")" != "$target_dir/debug/harn" ]]; then
  echo "hook_export_fresh_worktree_harn_bin did not ignore the stale HARN_BIN" >&2
  cat "$tmp_root/fresh-hook-harn-path.txt" >&2
  exit 1
fi
if ! grep -Fxq "args=build --quiet --bin harn" "$record"; then
  echo "hook_export_fresh_worktree_harn_bin did not build a fresh harn binary" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$target_dir" "$record"; then
  echo "hook_export_fresh_worktree_harn_bin did not reuse CARGO_TARGET_DIR for Cargo intermediates" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "RUSTC_WRAPPER=" "$record"; then
  echo "hook_export_fresh_worktree_harn_bin did not clear RUSTC_WRAPPER" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_RUSTC_WRAPPER=" "$record"; then
  echo "hook_export_fresh_worktree_harn_bin did not clear CARGO_BUILD_RUSTC_WRAPPER" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
custom_build_dir="$tmp_root/custom build dir"
(
  cd "$hook_repo"
  # shellcheck source=/dev/null
  . ./.githooks/lib.sh
  RUSTC_WRAPPER=sccache \
    CARGO_BUILD_RUSTC_WRAPPER=sccache \
    CARGO_TARGET_DIR="$target_dir" \
    CARGO_BUILD_BUILD_DIR="$custom_build_dir" \
    FAKE_CARGO_RECORD="$record" \
    PATH="$fake_bin:$PATH" \
    hook_ensure_harn > "$tmp_root/hook-harn-path-preserve-build-dir.txt"
)

if [[ "$(cat "$tmp_root/hook-harn-path-preserve-build-dir.txt")" != "$target_dir/debug/harn" ]]; then
  echo "hook_ensure_harn did not preserve CARGO_TARGET_DIR with a custom build dir" >&2
  cat "$tmp_root/hook-harn-path-preserve-build-dir.txt" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$custom_build_dir" "$record"; then
  echo "hook_ensure_harn did not preserve explicit CARGO_BUILD_BUILD_DIR" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
prompt_record="$tmp_root/prompt-harn-record.txt"
RUSTC_WRAPPER=sccache \
  CARGO_BUILD_RUSTC_WRAPPER=sccache \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_HARN_RECORD="$prompt_record" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/check_no_rust_prompt_prose.sh"

if [[ "$(grep -c '^args=build --quiet --bin harn$' "$record")" -ne 1 ]]; then
  echo "check_no_rust_prompt_prose did not build harn exactly once" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "RUSTC_WRAPPER=" "$record"; then
  echo "check_no_rust_prompt_prose did not clear RUSTC_WRAPPER" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_RUSTC_WRAPPER=" "$record"; then
  echo "check_no_rust_prompt_prose did not clear CARGO_BUILD_RUSTC_WRAPPER" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$target_dir" "$record"; then
  echo "check_no_rust_prompt_prose did not reuse CARGO_TARGET_DIR for Cargo intermediates" >&2
  cat "$record" >&2
  exit 1
fi
if [[ "$(grep -c '^run scripts/check_rust_prompt_prose.harn$' "$prompt_record")" -ne 1 ]]; then
  echo "check_no_rust_prompt_prose did not run exactly one normal prompt-prose scan" >&2
  cat "$prompt_record" >&2
  exit 1
fi

: > "$record"
: > "$prompt_record"
RUSTC_WRAPPER=sccache \
  CARGO_BUILD_RUSTC_WRAPPER=sccache \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_HARN_RECORD="$prompt_record" \
  HARN_PROMPT_PROSE_SELF_TEST=1 \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/check_no_rust_prompt_prose.sh"

if [[ "$(grep -c '^run scripts/check_rust_prompt_prose.harn' "$prompt_record")" -ne 2 ]]; then
  echo "check_no_rust_prompt_prose did not run self-test plus scan when requested" >&2
  cat "$prompt_record" >&2
  exit 1
fi
if ! grep -Fxq "run scripts/check_rust_prompt_prose.harn -- --self-test" "$prompt_record"; then
  echo "check_no_rust_prompt_prose did not pass the self-test flag" >&2
  cat "$prompt_record" >&2
  exit 1
fi

echo "hook_harn_build_env_test: ok"
