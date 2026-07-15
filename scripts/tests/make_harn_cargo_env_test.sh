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
    printf '{"target_directory":"%s"}\n' "$FAKE_METADATA_TARGET_DIR"
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

cat > "$fake_bin/python3" <<'SH'
#!/usr/bin/env bash
echo "python3 must not run" >&2
exit 42
SH
chmod +x "$fake_bin/python3"

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
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$target_dir" "$record"; then
  echo "wrapper did not reuse CARGO_TARGET_DIR for Cargo intermediates" >&2
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
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$metadata_target_dir" "$record"; then
  echo "wrapper did not reuse metadata target_directory for Cargo intermediates" >&2
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
make_fmt_with_bin="$tmp_root/make-fmt-with-bin.txt"
make -C "$repo_root" -n check-highlight check-protocol-artifacts > "$make_no_bin"
for expected in \
  './scripts/harn_bin.sh -- dump-highlight-keywords --check' \
  './scripts/harn_bin.sh -- dump-protocol-artifacts --check'
do
  if ! grep -Fq "$expected" "$make_no_bin"; then
    echo "Makefile HARN_CLI_CMD did not use the fresh harn-bin wrapper without HARN_BIN: $expected" >&2
    cat "$make_no_bin" >&2
    exit 1
  fi
done

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
make -C "$repo_root" -n check-highlight check-protocol-artifacts HARN_BIN="$fake_harn" > "$make_with_bin"
for expected in \
  "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- dump-highlight-keywords --check" \
  "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- dump-protocol-artifacts --check"
do
  if ! grep -Fq "$expected" "$make_with_bin"; then
    echo "Makefile HARN_CLI_CMD did not preserve HARN_BIN fast path: $expected" >&2
    cat "$make_with_bin" >&2
    exit 1
  fi
done
if grep -Fq './scripts/cargo_with_worktree_build_dir.sh run' "$make_with_bin"; then
  echo "Makefile used wrapper despite HARN_BIN being set" >&2
  cat "$make_with_bin" >&2
  exit 1
fi

make -C "$repo_root" -n fmt-harn HARN_BIN="$fake_harn" > "$make_fmt_with_bin"
if ! grep -Fq "xargs -0 env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- fmt --check" "$make_fmt_with_bin"; then
  echo "fmt-harn is not xargs-safe when HARN_BIN is set" >&2
  cat "$make_fmt_with_bin" >&2
  exit 1
fi

make_provider_targets="$tmp_root/make-provider-targets.txt"
make -C "$repo_root" -n \
  gen-provider-catalog check-provider-catalog \
  gen-provider-support check-provider-support \
  gen-provider-matrix check-provider-matrix \
  HARN_BIN="$fake_harn" > "$make_provider_targets"

provider_resolutions=$(grep -Fc './scripts/harn_bin.sh --print' "$make_provider_targets")
if [ "$provider_resolutions" -ne 4 ]; then
  echo "provider composed targets should resolve HARN_BIN once per recipe, got $provider_resolutions" >&2
  cat "$make_provider_targets" >&2
  exit 1
fi

for expected in \
  'env HARN_BIN="'"$fake_harn"'" ./scripts/harn_bin.sh -- provider catalog generate' \
  'env HARN_BIN="'"$fake_harn"'" ./scripts/harn_bin.sh -- provider catalog generate --check' \
  '"$harn_bin" provider catalog generate;' \
  '"$harn_bin" provider catalog support' \
  '"$harn_bin" provider catalog generate --check;' \
  '"$harn_bin" provider catalog support --check' \
  '"$harn_bin" provider catalog matrix' \
  '"$harn_bin" provider catalog matrix --check'
do
  if ! grep -Fq "$expected" "$make_provider_targets"; then
    echo "provider composed target did not reuse the resolved harn binary: $expected" >&2
    cat "$make_provider_targets" >&2
    exit 1
  fi
done
if grep -Eq 'make.*gen-provider-(config|capabilities)' "$make_provider_targets"; then
  echo "provider composed targets still recurse through sub-targets" >&2
  cat "$make_provider_targets" >&2
  exit 1
fi
echo "make_harn_cargo_env_test: ok"
