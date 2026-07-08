#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
target_dir="$tmp_root/target dir"
metadata_target_dir="$tmp_root/metadata target dir"
record="$tmp_root/cargo-record.txt"
mkdir -p "$fake_bin" "$target_dir" "$metadata_target_dir"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'args=%s\n' "$*"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-__unset__}"
  printf 'CARGO_BUILD_BUILD_DIR=%s\n' "${CARGO_BUILD_BUILD_DIR-__unset__}"
} >> "$FAKE_CARGO_RECORD"
case "$*" in
  "metadata --format-version=1 --no-deps")
    python3 - <<'PY'
import json
import os
print(json.dumps({"target_directory": os.environ["FAKE_METADATA_TARGET_DIR"]}))
PY
    ;;
  run\ *)
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_bin/cargo"

PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_METADATA_TARGET_DIR="$metadata_target_dir" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" \
    run --quiet -p harn-cli -- dump-highlight-keywords --check

if ! grep -Fxq "args=run --quiet -p harn-cli -- dump-highlight-keywords --check" "$record"; then
  echo "wrapper did not pass cargo arguments through" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_TARGET_DIR=$target_dir" "$record"; then
  echo "wrapper did not preserve CARGO_TARGET_DIR" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$target_dir/build" "$record"; then
  echo "wrapper did not default CARGO_BUILD_BUILD_DIR under CARGO_TARGET_DIR" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
custom_build_dir="$tmp_root/custom build dir"
PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  CARGO_BUILD_BUILD_DIR="$custom_build_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_METADATA_TARGET_DIR="$metadata_target_dir" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" run --bin harn -- check foo.harn

if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$custom_build_dir" "$record"; then
  echo "wrapper did not preserve explicit CARGO_BUILD_BUILD_DIR" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
PATH="$fake_bin:$PATH" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_METADATA_TARGET_DIR="$metadata_target_dir" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" run --quiet --bin harn -- fmt --check

if ! grep -Fxq "args=metadata --format-version=1 --no-deps" "$record"; then
  echo "wrapper did not query cargo metadata when CARGO_TARGET_DIR was unset" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_TARGET_DIR=$metadata_target_dir" "$record"; then
  echo "wrapper did not export metadata target_directory" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$metadata_target_dir/build" "$record"; then
  echo "wrapper did not isolate build dir under metadata target_directory" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
printf 'example.harn\0' \
  | PATH="$fake_bin:$PATH" \
    CARGO_TARGET_DIR="$target_dir" \
    FAKE_CARGO_RECORD="$record" \
    FAKE_METADATA_TARGET_DIR="$metadata_target_dir" \
    xargs -0 "$repo_root/scripts/cargo_with_worktree_build_dir.sh" run --quiet --bin harn -- fmt

if ! grep -Fxq "args=run --quiet --bin harn -- fmt example.harn" "$record"; then
  echo "wrapper is not directly usable from xargs callers" >&2
  cat "$record" >&2
  exit 1
fi

make_no_bin="$tmp_root/make-no-bin.txt"
make_with_bin="$tmp_root/make-with-bin.txt"
make -C "$repo_root" -n check-highlight > "$make_no_bin"
if ! grep -Fq './scripts/cargo_with_worktree_build_dir.sh run --quiet -p harn-cli -- dump-highlight-keywords --check' "$make_no_bin"; then
  echo "Makefile HARN_CLI_CMD did not use the Cargo env wrapper without HARN_BIN" >&2
  cat "$make_no_bin" >&2
  exit 1
fi

make_targets="$tmp_root/make-cargo-targets.txt"
make -C "$repo_root" -n \
  build build-release fmt fmt-check lint test-cargo test-e2e release-smoke \
  mcp-rc-conformance lint-harn gen-run-view-fixtures check-run-view-fixtures > "$make_targets"

for expected in \
  './scripts/cargo_with_worktree_build_dir.sh build' \
  './scripts/cargo_with_worktree_build_dir.sh build --release' \
  './scripts/cargo_with_worktree_build_dir.sh fmt --all' \
  './scripts/cargo_with_worktree_build_dir.sh fmt --all -- --check' \
  './scripts/cargo_with_worktree_build_dir.sh clippy --workspace --all-targets -- -D warnings' \
  './scripts/cargo_with_worktree_build_dir.sh test --workspace' \
  './scripts/cargo_with_worktree_build_dir.sh nextest run --workspace --profile e2e' \
  './scripts/cargo_with_worktree_build_dir.sh build --release -p harn-cli --bin harn' \
  './scripts/cargo_with_worktree_build_dir.sh test -p harn-mcp-rc-compat --tests' \
  './scripts/cargo_with_worktree_build_dir.sh test -p harn-cli --lib mcp_rc_compat_tests' \
  './scripts/cargo_with_worktree_build_dir.sh build --quiet --bin harn' \
  './scripts/cargo_with_worktree_build_dir.sh test -p harn-vm --test run_view_fixtures -- run_view_fixture_snapshots_match --exact'
do
  if ! grep -Fq "$expected" "$make_targets"; then
    echo "Makefile target did not use the Cargo env wrapper: $expected" >&2
    cat "$make_targets" >&2
    exit 1
  fi
done

fake_harn="$tmp_root/fake harn"
touch "$fake_harn"
chmod +x "$fake_harn"
make -C "$repo_root" -n check-highlight HARN_BIN="$fake_harn" > "$make_with_bin"
if ! grep -Fq "\"$fake_harn\" dump-highlight-keywords --check" "$make_with_bin"; then
  echo "Makefile HARN_CLI_CMD did not preserve HARN_BIN fast path" >&2
  cat "$make_with_bin" >&2
  exit 1
fi
if grep -Fq './scripts/cargo_with_worktree_build_dir.sh' "$make_with_bin"; then
  echo "Makefile used wrapper despite HARN_BIN being set" >&2
  cat "$make_with_bin" >&2
  exit 1
fi

echo "make_harn_cargo_env_test: ok"
