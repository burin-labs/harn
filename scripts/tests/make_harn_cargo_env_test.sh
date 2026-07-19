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
    if [[ -n "${FAKE_METADATA_JSON:-}" ]]; then
      printf '%s\n' "$FAKE_METADATA_JSON"
    else
      printf '{"target_directory":"%s"}\n' "$FAKE_METADATA_TARGET_DIR"
    fi
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

cat > "$fake_bin/harn-lease" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
for name in HARN_CARGO_LEASE_RUNNER HARN_CARGO_LEASE_OWNER HARN_CARGO_LEASE_HOST \
  HARN_CARGO_LEASE_WAIT_MS HARN_CARGO_LEASE_PRIORITY_CLASS; do
  if [[ -v "$name" ]]; then
    echo "wrapper leaked $name into the lease runner" >&2
    exit 91
  fi
done
printf '%s\n' "$@" > "$FAKE_HARN_LEASE_RECORD"
SH
chmod +x "$fake_bin/harn-lease"

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

lease_record="$tmp_root/harn-lease-record.txt"
PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  CARGO_BUILD_BUILD_DIR="$custom_build_dir" \
  HARN_CARGO_LEASE_RUNNER=harn-lease \
  HARN_CARGO_LEASE_OWNER=lease-test \
  HARN_CARGO_LEASE_HOST=mac-local \
  HARN_CARGO_LEASE_WAIT_MS=123 \
  HARN_CARGO_LEASE_PRIORITY_CLASS=interactive \
  FAKE_HARN_LEASE_RECORD="$lease_record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" test -p harn-vm

cat > "$tmp_root/expected-harn-lease-record.txt" <<EOF
host
lease
run
cargo
--owner
lease-test
--workspace
$repo_root
--target-dir
$target_dir
--build-dir
$custom_build_dir
--host
mac-local
--wait-ms
123
--priority-class
interactive
--
test
-p
harn-vm
EOF
if ! diff -u "$tmp_root/expected-harn-lease-record.txt" "$lease_record"; then
  echo "wrapper did not route the isolated Cargo invocation through Harn" >&2
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
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=__unset__" "$record"; then
  echo "wrapper overrode Cargo config for a metadata-discovered target_directory" >&2
  cat "$record" >&2
  exit 1
fi

real_workspace="$tmp_root/real-workspace"
configured_target="$tmp_root/configured target dir"
configured_build="$tmp_root/configured build dir"
mkdir -p "$real_workspace/.cargo" "$real_workspace/src"
cat > "$real_workspace/Cargo.toml" <<'TOML'
[package]
name = "shared-build-dir-proof"
version = "0.1.0"
edition = "2021"
TOML
cat > "$real_workspace/src/lib.rs" <<'RS'
pub fn proof() -> bool {
    true
}
RS
cat > "$real_workspace/.cargo/config.toml" <<TOML
[build]
target-dir = "$configured_target"
build-dir = "$configured_build"
TOML
(
  cd "$real_workspace"
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" check --quiet
)
if [[ ! -d "$configured_build/debug/.fingerprint" ]]; then
  echo "wrapper did not preserve Cargo's configured shared build-dir" >&2
  find "$configured_target" "$configured_build" -maxdepth 3 -print >&2 || true
  exit 1
fi
if [[ -d "$configured_target/debug/.fingerprint" ]]; then
  echo "metadata-discovered target unexpectedly received intermediate artifacts" >&2
  find "$configured_target" "$configured_build" -maxdepth 3 -print >&2 || true
  exit 1
fi

: > "$record"
set +e
PATH="$fake_bin:$PATH" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_METADATA_JSON='{"packages":[]}' \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" run --quiet --bin harn -- fmt --check \
  > "$tmp_root/missing-target-stdout.txt" \
  2> "$tmp_root/missing-target-stderr.txt"
missing_target_status=$?
set -e
if [[ "$missing_target_status" -eq 0 ]]; then
  echo "wrapper accepted cargo metadata without target_directory" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "cargo metadata did not report a simple target_directory" "$tmp_root/missing-target-stderr.txt"; then
  echo "wrapper did not explain missing target_directory" >&2
  cat "$tmp_root/missing-target-stderr.txt" >&2
  exit 1
fi
if grep -Fxq "args=run --quiet --bin harn -- fmt --check" "$record"; then
  echo "wrapper ran cargo after malformed metadata" >&2
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
  './scripts/cargo_with_worktree_build_dir.sh nextest run --workspace --profile e2e --run-ignored all' \
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

for variable in \
  HARN_EGRESS_ALLOW \
  HARN_EGRESS_DENY \
  HARN_EGRESS_DEFAULT \
  HARN_EGRESS_BLOCK_PRIVATE \
  HARN_EGRESS_ALLOW_LOOPBACK
do
  if ! grep -Fq -- "-u $variable" "$make_targets"; then
    echo "Makefile Rust test targets did not clear ambient $variable" >&2
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

source_only_targets="$tmp_root/source-only-targets.txt"
make -C "$repo_root" -n \
  sync-language-spec check-language-spec \
  check-source-file-lengths update-source-file-length-baseline \
  > "$source_only_targets"
for expected in \
  './scripts/harn_bin.sh --no-build -- run scripts/sync_language_spec.harn' \
  './scripts/harn_bin.sh --no-build -- run scripts/sync_language_spec.harn -- --check' \
  './scripts/harn_bin.sh --no-build -- run scripts/check_source_file_lengths.harn' \
  './scripts/harn_bin.sh --no-build -- run scripts/check_source_file_lengths.harn -- --update'
do
  if ! grep -Fq "$expected" "$source_only_targets"; then
    echo "source-only Make target can still trigger an implicit build: $expected" >&2
    cat "$source_only_targets" >&2
    exit 1
  fi
done

unset HARN_BIN HARN_BIN_NO_BUILD
missing_harn_target="$tmp_root/missing harn target"
run_no_build_gate() {
  name="$1"
  shift
  : > "$record"
  if PATH="$fake_bin:$PATH" \
    CARGO_TARGET_DIR="$missing_harn_target" \
    FAKE_CARGO_RECORD="$record" \
    FAKE_METADATA_TARGET_DIR="$metadata_target_dir" \
    "$@" > "$tmp_root/$name.out" 2> "$tmp_root/$name.err"; then
    echo "$name unexpectedly passed without a Harn binary" >&2
    exit 1
  fi
  if ! grep -Fq "no fresh worktree harn binary found" "$tmp_root/$name.err"; then
    echo "$name did not report its missing no-build Harn binary" >&2
    cat "$tmp_root/$name.err" >&2
    exit 1
  fi
  if [[ -s "$record" ]]; then
    echo "$name invoked Cargo despite its no-build contract" >&2
    cat "$record" >&2
    exit 1
  fi
}

run_no_build_gate sync-language-spec make -C "$repo_root" sync-language-spec
run_no_build_gate check-source-file-lengths make -C "$repo_root" check-source-file-lengths
run_no_build_gate check-stdlib-strict-types \
  "$repo_root/scripts/check_stdlib_strict_types.sh"
run_no_build_gate check-stdlib-public-return-types \
  "$repo_root/scripts/check_stdlib_public_return_types.sh"

echo "make_harn_cargo_env_test: ok"
