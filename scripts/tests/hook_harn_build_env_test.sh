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
if [[ "${1:-}" = "__internal-freshness-evidence-v5" ]]; then
  if [[ -n "${7:-}" ]]; then printf "harn-freshness-manifest-v4\n" >"$7"; fi
  binary_hash="$(git hash-object --no-filters -- "$3")000000000000000000000000"
  dep_hash="$(git hash-object --no-filters -- "$2")000000000000000000000000"
  printf 'harn-artifact-evidence-v5-cargo-output-dep-info-v1-manifest-3\nbuild-freshness=%s\nbuild-id=%s\nartifact-stat=%s\ndep-info=%s\ndependencies=%s\n' \
    "$(cat "$3.build-freshness")" "$binary_hash" "$binary_hash" \
    "$dep_hash" "$dep_hash"
  exit 0
fi
printf '%s\n' "$*" >> "$FAKE_HARN_RECORD"
BIN
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    printf '%s\n' "${HARN_BUILD_FRESHNESS_ID:?}" \
      > "$CARGO_TARGET_DIR/debug/harn.build-freshness"
    escaped_harn="${CARGO_TARGET_DIR// /\\ }/debug/harn"
    printf '%s:\n' "$escaped_harn" > "$CARGO_TARGET_DIR/debug/harn.d"
    ;;
  "build --quiet --bin harn --bin harn-freshness-check --features internal-freshness-checker")
    mkdir -p "${CARGO_TARGET_DIR:?}/debug"
    cat > "$CARGO_TARGET_DIR/debug/harn" <<'BIN'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" = "__internal-freshness-evidence-v5" ]]; then
  if [[ -n "${7:-}" ]]; then printf "harn-freshness-manifest-v4\n" >"$7"; fi
  binary_hash="$(git hash-object --no-filters -- "$3")000000000000000000000000"
  dep_hash="$(git hash-object --no-filters -- "$2")000000000000000000000000"
  printf 'harn-artifact-evidence-v5-cargo-output-dep-info-v1-manifest-3\nbuild-freshness=%s\nbuild-id=%s\nartifact-stat=%s\ndep-info=%s\ndependencies=%s\n' \
    "$(cat "$3.build-freshness")" "$binary_hash" "$binary_hash" \
    "$dep_hash" "$dep_hash"
  exit 0
fi
if [[ "${1:-}" = "__internal-executable-path" ]]; then
  printf '%s\n' "$0"
  exit 0
fi
printf '%s\n' "$*" >> "$FAKE_HARN_RECORD"
BIN
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    cp "$FAKE_FRESHNESS_CHECKER" "$CARGO_TARGET_DIR/debug/harn-freshness-check"
    chmod +x "$CARGO_TARGET_DIR/debug/harn-freshness-check"
    printf '%s\n' "${HARN_BUILD_FRESHNESS_ID:?}" \
      > "$CARGO_TARGET_DIR/debug/harn.build-freshness"
    escaped_harn="${CARGO_TARGET_DIR// /\\ }/debug/harn"
    printf '%s:\n' "$escaped_harn" > "$CARGO_TARGET_DIR/debug/harn.d"
    ;;
  "metadata --format-version=1 --no-deps")
    printf '{"target_directory":"%s"}\n' "$CARGO_TARGET_DIR"
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_bin/cargo"
export FAKE_FRESHNESS_CHECKER="$repo_root/scripts/tests/fixtures/harn_bin/fake_freshness_checker.sh"
# Resolver tests below own both the explicit binary and build policy. Clear the
# pair so callers that intentionally reuse a prebuilt Harn binary cannot change
# the fake-Cargo path under test.
unset HARN_BIN HARN_BIN_NO_BUILD
export HARN_CARGO_LEASE_MODE=off

hook_repo="$tmp_root/hook-repo"
mkdir -p "$hook_repo/.githooks" "$hook_repo/scripts/lib"
cp "$repo_root/.githooks/lib.sh" "$hook_repo/.githooks/lib.sh"
cp "$repo_root/scripts/lib/cargo_env.sh" "$hook_repo/scripts/lib/cargo_env.sh"
cp "$repo_root/scripts/lib/harn_bin.sh" "$hook_repo/scripts/lib/harn_bin.sh"
cp "$repo_root/scripts/lib/harn_bin_freshness.sh" \
  "$hook_repo/scripts/lib/harn_bin_freshness.sh"
cp "$repo_root/scripts/harn_bin.sh" "$hook_repo/scripts/harn_bin.sh"
cp "$repo_root/scripts/cargo_with_worktree_build_dir.sh" \
  "$hook_repo/scripts/cargo_with_worktree_build_dir.sh"
git -C "$hook_repo" init --quiet
git -C "$hook_repo" config user.name 'Harn Hook Test'
git -C "$hook_repo" config user.email 'harn-hook-test@example.invalid'
git -C "$hook_repo" config commit.gpgsign false
git -C "$hook_repo" add .githooks scripts
git -C "$hook_repo" commit -qm fixture

# Git Bash resolves `harn` to `harn.exe` for executable tests. Reproduce that
# ambiguous surface with both spellings present: Windows selection must be
# structural so receipt suffixes remain bound to the exact artifact filename.
windows_target="$tmp_root/windows target"
mkdir -p "$windows_target/debug"
for name in harn harn.exe; do
  cp "$fake_bin/cargo" "$windows_target/debug/$name"
  chmod +x "$windows_target/debug/$name"
  : > "$windows_target/debug/$name.freshness"
  : > "$windows_target/debug/$name.freshness.manifest"
done
for name in harn-freshness-check harn-freshness-check.exe; do
  cp "$FAKE_FRESHNESS_CHECKER" "$windows_target/debug/$name"
  chmod +x "$windows_target/debug/$name"
done
windows_hook_bin="$(
  cd "$hook_repo"
  # shellcheck source=/dev/null
  . ./.githooks/lib.sh
  OS=Windows_NT CARGO_TARGET_DIR="$windows_target" \
    hook_find_fresh_worktree_harn_bin
)"
if [[ "$windows_hook_bin" != "$windows_target/debug/harn.exe" ]]; then
  echo "Windows hook selection did not preserve the native executable suffix" >&2
  printf 'selected=%q expected=%q\n' \
    "$windows_hook_bin" "$windows_target/debug/harn.exe" >&2
  exit 1
fi

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

if ! grep -Fxq "args=build --quiet --bin harn --bin harn-freshness-check --features internal-freshness-checker" "$record"; then
  echo "hook_ensure_harn did not resolve the harn binary through the exact wrapper" >&2
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
if [[ -s "$record" ]]; then
  echo "hook_export_fresh_worktree_harn_bin rebuilt despite an exact fresh receipt" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
custom_build_dir="$tmp_root/custom build dir"
rm -f "$target_dir/debug/harn.freshness"
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

if [[ "$(grep -c '^args=build --quiet --bin harn --bin harn-freshness-check --features internal-freshness-checker$' "$record")" -ne 1 ]]; then
  echo "check_no_rust_prompt_prose did not resolve harn through Cargo exactly once" >&2
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
