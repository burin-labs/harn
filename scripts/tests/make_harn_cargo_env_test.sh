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
  build\ *|clean|fmt\ *|run\ *|test\ *)
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
  HARN_CARGO_LEASE_WAIT_MS HARN_CARGO_LEASE_PRIORITY_CLASS HARN_CARGO_LEASE_MODE; do
  if [[ -v "$name" ]]; then
    echo "wrapper leaked $name into the lease runner" >&2
    exit 91
  fi
done
printf 'CARGO_HARN_HOST_LEASE_ACTIVE=%s\n' \
  "${CARGO_HARN_HOST_LEASE_ACTIVE-__unset__}" > "$FAKE_HARN_LEASE_RECORD"
printf '%s\n' "$@" >> "$FAKE_HARN_LEASE_RECORD"
SH
chmod +x "$fake_bin/harn-lease"

cat > "$fake_bin/python3" <<'SH'
#!/usr/bin/env bash
echo "python3 must not run" >&2
exit 42
SH
chmod +x "$fake_bin/python3"

# Direct-wrapper fixtures below exercise target/build-dir behavior, not machine
# admission. Opt out explicitly so a developer's installed harn cannot change
# their path through the fixture.
export HARN_CARGO_LEASE_MODE=off

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
env -u HARN_CARGO_LEASE_MODE \
  PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  CARGO_BUILD_BUILD_DIR="$custom_build_dir" \
  HARN_CARGO_LEASE_RUNNER=harn-lease \
  HARN_CARGO_LEASE_OWNER=lease-test \
  HARN_CARGO_LEASE_HOST=mac-local \
  HARN_CARGO_LEASE_WAIT_MS=123 \
  HARN_CARGO_LEASE_PRIORITY_CLASS=interactive \
  CI=true \
  FAKE_HARN_LEASE_RECORD="$lease_record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" --locked test -p harn-vm

cat > "$tmp_root/expected-harn-lease-record.txt" <<EOF
CARGO_HARN_HOST_LEASE_ACTIVE=1
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
--locked
test
-p
harn-vm
EOF
if ! diff -u "$tmp_root/expected-harn-lease-record.txt" "$lease_record"; then
  echo "wrapper did not route the isolated Cargo invocation through Harn" >&2
  exit 1
fi

auto_harn="$target_dir/debug/harn"
mkdir -p "$(dirname "$auto_harn")"
cp "$fake_bin/harn-lease" "$auto_harn"
: > "$lease_record"
env -u HARN_CARGO_LEASE_MODE -u CI \
  PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_HARN_LEASE_RECORD="$lease_record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" test -p harn-vm

cat > "$tmp_root/expected-auto-harn-lease-record.txt" <<EOF
CARGO_HARN_HOST_LEASE_ACTIVE=1
host
lease
run
cargo
--owner
cargo-wrapper
--workspace
$repo_root
--target-dir
$target_dir
--build-dir
$target_dir
--wait-ms
3600000
--priority-class
interactive
--
test
-p
harn-vm
EOF
if ! diff -u "$tmp_root/expected-auto-harn-lease-record.txt" "$lease_record"; then
  echo "wrapper did not auto-route a heavy Cargo command through the target Harn binary" >&2
  exit 1
fi

cp "$fake_bin/harn-lease" "$fake_bin/harn"
cat > "$target_dir/debug/harn.exe" <<'SH'
#!/usr/bin/env bash
echo "Windows wrapper tried to supervise a build with its target executable" >&2
exit 97
SH
chmod +x "$target_dir/debug/harn.exe"
: > "$lease_record"
env -u HARN_CARGO_LEASE_MODE -u CI \
  OS=Windows_NT \
  PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_HARN_LEASE_RECORD="$lease_record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" test -p harn-vm
if ! diff -u "$tmp_root/expected-auto-harn-lease-record.txt" "$lease_record"; then
  echo "Windows wrapper did not use the independently installed Harn runner" >&2
  exit 1
fi
rm -f "$fake_bin/harn" "$target_dir/debug/harn.exe"

: > "$record"
: > "$lease_record"
HARN_CARGO_LEASE_MODE=required \
  HARN_CARGO_LEASE_RUNNER=harn-lease \
  PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  CARGO_HARN_HOST_LEASE_ACTIVE=1 \
  FAKE_CARGO_RECORD="$record" \
  FAKE_HARN_LEASE_RECORD="$lease_record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" test -p harn-vm
if ! grep -Fxq "args=test -p harn-vm" "$record" || [[ -s "$lease_record" ]]; then
  echo "nested wrapper call reacquired its active rust-heavy lease" >&2
  cat "$record" "$lease_record" >&2
  exit 1
fi

: > "$record"
: > "$lease_record"
HARN_CARGO_LEASE_MODE=required \
  HARN_CARGO_LEASE_RUNNER=harn-lease \
  PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_HARN_LEASE_RECORD="$lease_record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" fmt --all
if ! grep -Fxq "args=fmt --all" "$record" || [[ -s "$lease_record" ]]; then
  echo "static Cargo command acquired the rust-heavy lease" >&2
  cat "$record" "$lease_record" >&2
  exit 1
fi

: > "$lease_record"
HARN_CARGO_LEASE_MODE=required \
  HARN_CARGO_LEASE_RUNNER=harn-lease \
  PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_HARN_LEASE_RECORD="$lease_record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" clean
if ! grep -Fxq "clean" "$lease_record"; then
  echo "cargo clean bypassed the rust-heavy lease" >&2
  cat "$lease_record" >&2
  exit 1
fi
rm -f "$auto_harn"

: > "$record"
env -u HARN_CARGO_LEASE_MODE -u CI \
  PATH="$fake_bin:/usr/bin:/bin" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" build -p harn-vm \
  > "$tmp_root/auto-fallback-stdout.txt" \
  2> "$tmp_root/auto-fallback-stderr.txt"
if ! grep -Fxq "args=build -p harn-vm" "$record"; then
  echo "automatic lease discovery did not fall back to Cargo" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "no compatible prebuilt Harn binary" "$tmp_root/auto-fallback-stderr.txt"; then
  echo "automatic lease fallback was silent" >&2
  cat "$tmp_root/auto-fallback-stderr.txt" >&2
  exit 1
fi

: > "$record"
if HARN_CARGO_LEASE_MODE=required \
  PATH="$fake_bin:/usr/bin:/bin" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" build -p harn-vm \
  > "$tmp_root/required-missing-stdout.txt" \
  2> "$tmp_root/required-missing-stderr.txt"; then
  echo "required lease mode accepted a missing runner" >&2
  exit 1
fi
if [[ -s "$record" ]] \
  || ! grep -Fq "no compatible prebuilt Harn binary" "$tmp_root/required-missing-stderr.txt"; then
  echo "required lease mode did not fail closed before Cargo" >&2
  cat "$record" "$tmp_root/required-missing-stderr.txt" >&2
  exit 1
fi

: > "$record"
if HARN_CARGO_LEASE_MODE=invalid \
  PATH="$fake_bin:/usr/bin:/bin" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" fmt --all \
  > "$tmp_root/invalid-mode-stdout.txt" \
  2> "$tmp_root/invalid-mode-stderr.txt"; then
  echo "wrapper accepted an invalid lease mode" >&2
  exit 1
fi
if [[ -s "$record" ]] \
  || ! grep -Fq "must be auto, off, or required" "$tmp_root/invalid-mode-stderr.txt"; then
  echo "invalid lease mode did not fail before Cargo" >&2
  cat "$record" "$tmp_root/invalid-mode-stderr.txt" >&2
  exit 1
fi

cp "$fake_bin/harn-lease" "$auto_harn"
: > "$record"
: > "$lease_record"
env -u HARN_CARGO_LEASE_MODE \
  CI=true \
  PATH="$fake_bin:$PATH" \
  CARGO_TARGET_DIR="$target_dir" \
  FAKE_CARGO_RECORD="$record" \
  FAKE_HARN_LEASE_RECORD="$lease_record" \
  "$repo_root/scripts/cargo_with_worktree_build_dir.sh" build -p harn-vm
if ! grep -Fxq "args=build -p harn-vm" "$record" || [[ -s "$lease_record" ]]; then
  echo "CI default unexpectedly acquired the local rust-heavy lease" >&2
  cat "$record" "$lease_record" >&2
  exit 1
fi
rm -f "$auto_harn"

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
  build build-release fmt fmt-check lint test test-cargo test-e2e release-smoke \
  mcp-conformance lint-harn gen-run-view-fixtures check-run-view-fixtures > "$make_targets"

for expected in \
  './scripts/cargo_with_worktree_build_dir.sh build' \
  './scripts/cargo_with_worktree_build_dir.sh build --release' \
  './scripts/cargo_with_worktree_build_dir.sh fmt --all' \
  './scripts/cargo_with_worktree_build_dir.sh fmt --all -- --check' \
  './scripts/cargo_with_worktree_build_dir.sh clippy --workspace --all-targets -- -D warnings' \
  './scripts/cargo_with_worktree_build_dir.sh nextest run --workspace' \
  './scripts/cargo_with_worktree_build_dir.sh test --workspace' \
  './scripts/cargo_with_worktree_build_dir.sh nextest run --workspace --profile e2e --run-ignored all' \
  './scripts/cargo_with_worktree_build_dir.sh build --release -p harn-cli --bin harn' \
  './scripts/cargo_with_worktree_build_dir.sh test -p harn-mcp-compat --tests' \
  './scripts/cargo_with_worktree_build_dir.sh test -p harn-cli --lib mcp_compat_tests'
do
  if ! grep -Fq "$expected" "$make_targets"; then
    echo "Makefile target did not use the Cargo env wrapper: $expected" >&2
    cat "$make_targets" >&2
    exit 1
  fi
done

for expected in \
  './scripts/harn_bin.sh -- session view-fixtures --write --repository-root .' \
  './scripts/harn_bin.sh -- session view-fixtures --check --repository-root .'
do
  if ! grep -Fq "$expected" "$make_targets"; then
    echo "run-view fixture target did not use the production Harn binary: $expected" >&2
    cat "$make_targets" >&2
    exit 1
  fi
done
if grep -Fq 'run_view_fixtures::run_view_fixture_snapshots_match' "$make_targets"; then
  echo "run-view fixture target regressed to a separate Cargo test graph" >&2
  cat "$make_targets" >&2
  exit 1
fi

make_focused_test="$tmp_root/make-focused-test.txt"
make -C "$repo_root" -n test ARGS='-p harn-vm typed_options_parity' > "$make_focused_test"
if ! grep -Fq \
  './scripts/cargo_with_worktree_build_dir.sh nextest run -p harn-vm typed_options_parity' \
  "$make_focused_test"; then
  echo "Makefile test target did not forward documented focused ARGS" >&2
  cat "$make_focused_test" >&2
  exit 1
fi
if grep -Fq 'nextest run --workspace -p harn-vm' "$make_focused_test"; then
  echo "focused Makefile test target retained --workspace and would compile every package" >&2
  cat "$make_focused_test" >&2
  exit 1
fi

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

if ! grep -Fq 'RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"' "$make_targets"; then
  echo "Makefile Rust test targets did not set the production CLI stack default" >&2
  cat "$make_targets" >&2
  exit 1
fi

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
if ! grep -Fq "find scripts -type d -name '.harn*' -prune -o -type f -name '*.harn' -print0" "$make_fmt_with_bin"; then
  echo "fmt-harn does not prune ignored runtime directories under scripts" >&2
  cat "$make_fmt_with_bin" >&2
  exit 1
fi

make_all="$tmp_root/make-all.txt"
make -C "$repo_root" -n all HARN_BIN="$fake_harn" > "$make_all"
if ! grep -Fq 'make HARN_BIN="$harn_bin" lint lint-md lint-actions' "$make_all"; then
  echo "all did not pass one resolved Harn binary to the parallel child gate" >&2
  cat "$make_all" >&2
  exit 1
fi
if ! grep -Fq './scripts/snapshot_harn_bin.sh "$harn_bin" "$stable_root/harn-bin"' "$make_all"; then
  echo "all did not snapshot the resolved Cargo output before parallel execution" >&2
  cat "$make_all" >&2
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
