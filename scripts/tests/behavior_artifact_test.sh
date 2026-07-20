#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/behavior_artifact.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p "$tmpdir/bin" "$tmpdir/target/debug" "$tmpdir/out" "$tmpdir/receipts" "$tmpdir/work"
cp "$repo_root/rust-toolchain.toml" "$tmpdir/work/rust-toolchain.toml"
cat > "$tmpdir/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  build)
    [[ "$*" == "build --locked --bin harn" ]] || exit 2
    : > "${CARGO_RECEIPTS:?}/build"
    ;;
  metadata)
    [[ "$*" == "metadata --format-version 1 --no-deps" ]] || exit 2
    printf '{"target_directory":"%s"}\n' "${FAKE_TARGET:?}"
    ;;
  nextest)
    if [[ "$#" -eq 2 && "$2" == "--version" ]]; then
      printf 'cargo-nextest %s (fake)\n' "${FAKE_NEXTEST_VERSION:-0.9.132}"
      exit 0
    fi
    if [[ "$#" -ne 10 || "$2" != "archive" || "$3" != "--locked" || \
      "$4" != "--workspace" || "$5" != "--profile" || "$6" != "ci" || \
      "$7" != "-E" || "$9" != "--archive-file" || -z "${10}" ]]; then
      printf 'unexpected cargo nextest argv:' >&2
      printf ' <%s>' "$@" >&2
      printf '\n' >&2
      exit 2
    fi
    if [[ "$8" != 'all()' ]]; then
      echo "unexpected nextest filter: $8" >&2
      exit 2
    fi
    printf 'tests archive\n' > "${10}"
    : > "${CARGO_RECEIPTS:?}/nextest-tests"
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$tmpdir/bin/cargo"
cat > "$tmpdir/bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "rev-parse --verify HEAD" ]]; then
  printf '%s\n' "${FAKE_COMMIT:?}"
  exit 0
fi
echo "unexpected git invocation: $*" >&2
exit 2
SH
chmod +x "$tmpdir/bin/git"
cat > "$tmpdir/bin/rustc" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == "-vV" ]] || exit 2
printf '%s\n' "${FAKE_RUSTC_IDENTITY:-rustc 1.95.0 (fake)}"
SH
chmod +x "$tmpdir/bin/rustc"
printf '#!/usr/bin/env bash\necho harn\n' > "$tmpdir/target/debug/harn"
chmod +x "$tmpdir/target/debug/harn"

commit=0123456789abcdef0123456789abcdef01234567
bundle="$tmpdir/out/behavior.tar.zst"
run_artifact() {
  (
    cd "$tmpdir/work"
    env \
      PATH="$tmpdir/bin:$PATH" \
      CARGO_RECEIPTS="$tmpdir/receipts" \
      FAKE_TARGET="$tmpdir/target" \
      FAKE_COMMIT="${FAKE_COMMIT_OVERRIDE:-$commit}" \
      FAKE_NEXTEST_VERSION="${FAKE_NEXTEST_VERSION_OVERRIDE:-0.9.132}" \
      FAKE_RUSTC_IDENTITY="${FAKE_RUSTC_IDENTITY_OVERRIDE:-rustc 1.95.0 (fake)}" \
      RUSTFLAGS="${RUSTFLAGS_OVERRIDE:--D warnings -Clink-arg=-fuse-ld=mold}" \
      CARGO_PROFILE_DEV_DEBUG="${DEV_DEBUG_OVERRIDE:-line-tables-only}" \
      HARN_VERIFY_RUST_RUNTIME="${VERIFY_RUNTIME_OVERRIDE:-0}" \
      HARN_BEHAVIOR_ARTIFACT_MAX_BYTES="${MAX_BYTES_OVERRIDE:-6442450944}" \
      "$script" "$@"
  )
}

expect_failure() {
  local description=$1
  shift
  if "$@" > /dev/null 2>&1; then
    echo "$description" >&2
    exit 1
  fi
}

run_artifact build "$bundle" "$commit"
test -f "$tmpdir/receipts/build"
test -f "$tmpdir/receipts/nextest-tests"

github_env="$tmpdir/github-env"
VERIFY_RUNTIME_OVERRIDE=1 run_artifact restore "$bundle" "$tmpdir/restored" "$commit" "$github_env"
"$tmpdir/restored/harn" | grep -Fxq harn
restored="$(cd "$tmpdir/restored" && pwd -P)"
grep -Fxq "HARN_BIN=$restored/harn" "$github_env"
tar --zstd -tf "$bundle" | sort | diff -u - <(printf '%s\n' \
  SHA256SUMS harn harn-tests.tar.zst manifest | sort)

expect_failure "restore accepted a bundle for the wrong commit" \
  run_artifact restore "$bundle" "$tmpdir/wrong-commit" fedcba9876543210fedcba9876543210fedcba98

FAKE_COMMIT_OVERRIDE=fedcba9876543210fedcba9876543210fedcba98 \
  expect_failure "build accepted a commit that did not match checkout HEAD" \
  run_artifact build "$tmpdir/out/wrong-source.tar.zst" "$commit"

FAKE_NEXTEST_VERSION_OVERRIDE=0.9.131 VERIFY_RUNTIME_OVERRIDE=1 \
  expect_failure "restore accepted the wrong nextest version" \
  run_artifact restore "$bundle" "$tmpdir/wrong-nextest" "$commit"

FAKE_RUSTC_IDENTITY_OVERRIDE='rustc 1.96.0 (wrong)' VERIFY_RUNTIME_OVERRIDE=1 \
  expect_failure "restore accepted the wrong Rust toolchain" \
  run_artifact restore "$bundle" "$tmpdir/wrong-rustc" "$commit"

RUSTFLAGS_OVERRIDE='-D warnings' VERIFY_RUNTIME_OVERRIDE=1 \
  expect_failure "restore accepted the wrong Rust flags" \
  run_artifact restore "$bundle" "$tmpdir/wrong-flags" "$commit"

expect_failure "restore overwrote an existing destination" \
  run_artifact restore "$bundle" "$tmpdir/restored" "$commit"

MAX_BYTES_OVERRIDE=1 \
  expect_failure "restore accepted an over-budget bundle" \
  run_artifact restore "$bundle" "$tmpdir/over-budget" "$commit"

mkdir "$tmpdir/tampered"
tar --zstd -xf "$bundle" -C "$tmpdir/tampered"
printf '\nchanged=true\n' >> "$tmpdir/tampered/manifest"
tar --zstd -cf "$tmpdir/out/altered-manifest.tar.zst" -C "$tmpdir/tampered" \
  harn-tests.tar.zst harn manifest SHA256SUMS
expect_failure "restore accepted an altered manifest" \
  run_artifact restore "$tmpdir/out/altered-manifest.tar.zst" "$tmpdir/altered-manifest" "$commit"

printf 'corrupt\n' >> "$tmpdir/tampered/harn-tests.tar.zst"
tar --zstd -cf "$tmpdir/out/corrupt-member.tar.zst" -C "$tmpdir/tampered" \
  harn-tests.tar.zst harn manifest SHA256SUMS
expect_failure "restore accepted corrupt archive bytes" \
  run_artifact restore "$tmpdir/out/corrupt-member.tar.zst" "$tmpdir/corrupt-member" "$commit"

tar --zstd -cf "$tmpdir/out/missing-member.tar.zst" -C "$tmpdir/tampered" \
  harn manifest SHA256SUMS
expect_failure "restore accepted a missing archive member" \
  run_artifact restore "$tmpdir/out/missing-member.tar.zst" "$tmpdir/missing-member" "$commit"

cp "$tmpdir/work/rust-toolchain.toml" "$tmpdir/work/rust-toolchain.toml.saved"
printf '[toolchain]\nchannel = "1.96.0"\n' > "$tmpdir/work/rust-toolchain.toml"
expect_failure "restore accepted a different pinned toolchain" \
  run_artifact restore "$bundle" "$tmpdir/wrong-toolchain-file" "$commit"
mv "$tmpdir/work/rust-toolchain.toml.saved" "$tmpdir/work/rust-toolchain.toml"

MAX_BYTES_OVERRIDE=1 \
  expect_failure "build accepted an over-budget bundle" \
  run_artifact build "$tmpdir/out/build-over-budget.tar.zst" "$commit"

echo "behavior_artifact_test: ok"
