#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/rust_artifact.sh"
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
    if [[ "$#" -eq 10 && "$2" == "archive" && "$3" == "--locked" && \
      "$4" == "--workspace" && "$5" == "--profile" && "$6" == "ci" && \
      "$7" == "-E" && "$8" == 'all()' && "$9" == "--archive-file" && -n "${10}" ]]; then
      printf 'tests archive\n' > "${10}"
      : > "${CARGO_RECEIPTS:?}/nextest-tests"
    elif [[ "$#" -eq 13 && "$2" == "archive" && "$3" == "--locked" && \
      "$4" == "-p" && "$5" == "harn-vm" && "$6" == "-p" && "$7" == "harn-hostlib" && \
      "$8" == "--profile" && "$9" == "ci" && "${10}" == "-E" && \
      "${11}" == '(package(harn-vm) and binary(harn_vm)) or (package(harn-hostlib) and (test(local_backend_execs_inside_session_outputs) or test(local_backend_timeout_is_enforced_without_shell_timeout_binary) or test(sandboxed_npm_install_resolves_file_tarball_dependency_offline)))' && \
      "${12}" == "--archive-file" && -n "${13}" ]]; then
      printf 'security tests archive\n' > "${13}"
      : > "${CARGO_RECEIPTS:?}/nextest-security"
    else
      printf 'unexpected cargo nextest argv:' >&2
      printf ' <%s>' "$@" >&2
      printf '\n' >&2
      exit 2
    fi
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
bundle="$tmpdir/out/workspace-tests.tar.zst"
cli_bundle="$tmpdir/out/harn-cli.tar.zst"
security_bundle="$tmpdir/out/harn-security.tar.zst"
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
      HARN_RUST_TEST_ARTIFACT_MAX_BYTES="${MAX_BYTES_OVERRIDE:-9663676416}" \
      HARN_SECURITY_ARTIFACT_MAX_BYTES="${SECURITY_MAX_BYTES_OVERRIDE:-1073741824}" \
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

run_artifact build-tests "$bundle" "$commit"
test -f "$tmpdir/receipts/build"
test -f "$tmpdir/receipts/nextest-tests"
rm -f "$tmpdir/receipts/build" "$tmpdir/receipts/nextest-tests"
run_artifact build-check-inputs "$cli_bundle" "$security_bundle" "$commit"
test -f "$tmpdir/receipts/build"
test ! -f "$tmpdir/receipts/nextest-tests"
test -f "$tmpdir/receipts/nextest-security"

github_env="$tmpdir/github-env"
VERIFY_RUNTIME_OVERRIDE=1 run_artifact restore-tests "$bundle" "$tmpdir/restored" "$commit" "$github_env"
"$tmpdir/restored/harn" | grep -Fxq harn
restored="$(cd "$tmpdir/restored" && pwd -P)"
grep -Fxq "HARN_BIN=$restored/harn" "$github_env"
tar --zstd -tf "$bundle" | sort | diff -u - <(printf '%s\n' \
  SHA256SUMS harn harn-tests.tar.zst manifest | sort)

cli_github_env="$tmpdir/cli-github-env"
run_artifact restore-cli "$cli_bundle" "$tmpdir/restored-cli" "$commit" "$cli_github_env"
"$tmpdir/restored-cli/harn" | grep -Fxq harn
restored_cli="$(cd "$tmpdir/restored-cli" && pwd -P)"
grep -Fxq "HARN_BIN=$restored_cli/harn" "$cli_github_env"
tar --zstd -tf "$cli_bundle" | sort | diff -u - <(printf '%s\n' \
  CLI_SHA256SUMS harn manifest | sort)

VERIFY_RUNTIME_OVERRIDE=1 run_artifact restore-security "$security_bundle" \
  "$tmpdir/restored-security" "$commit"
grep -Fxq 'security tests archive' "$tmpdir/restored-security/harn-security-tests.tar.zst"
tar --zstd -tf "$security_bundle" | sort | diff -u - <(printf '%s\n' \
  SHA256SUMS harn-security-tests.tar.zst manifest | sort)
expect_failure "security restore accepted a bundle for the wrong commit" \
  run_artifact restore-security "$security_bundle" "$tmpdir/security-wrong-commit" \
  fedcba9876543210fedcba9876543210fedcba98
expect_failure "security restore overwrote an existing destination" \
  run_artifact restore-security "$security_bundle" "$tmpdir/restored-security" "$commit"
SECURITY_MAX_BYTES_OVERRIDE=1 \
  expect_failure "security restore accepted an over-budget bundle" \
  run_artifact restore-security "$security_bundle" "$tmpdir/security-over-budget" "$commit"

mkdir "$tmpdir/tampered-security"
tar --zstd -xf "$security_bundle" -C "$tmpdir/tampered-security"
printf '\nchanged=true\n' >> "$tmpdir/tampered-security/manifest"
tar --zstd -cf "$tmpdir/out/altered-security-manifest.tar.zst" -C "$tmpdir/tampered-security" \
  harn-security-tests.tar.zst manifest SHA256SUMS
expect_failure "security restore accepted an altered manifest" \
  run_artifact restore-security "$tmpdir/out/altered-security-manifest.tar.zst" \
  "$tmpdir/altered-security-manifest" "$commit"

# CLI-only producer mode: builds just the harn binary bundle, restorable by
# the same restore-cli consumers.
cli_only_bundle="$tmpdir/out/cli-only.tar.zst"
run_artifact build-cli "$cli_only_bundle" "$commit"
run_artifact restore-cli "$cli_only_bundle" "$tmpdir/restored-cli-only" "$commit"
"$tmpdir/restored-cli-only/harn" | grep -Fxq harn
tar --zstd -tf "$cli_only_bundle" | sort | diff -u - <(printf '%s\n' \
  CLI_SHA256SUMS harn manifest | sort)
FAKE_COMMIT_OVERRIDE=fedcba9876543210fedcba9876543210fedcba98 \
  expect_failure "build-cli accepted a commit that did not match checkout HEAD" \
  run_artifact build-cli "$tmpdir/out/cli-only-wrong.tar.zst" "$commit"

expect_failure "restore accepted a bundle for the wrong commit" \
  run_artifact restore-tests "$bundle" "$tmpdir/wrong-commit" fedcba9876543210fedcba9876543210fedcba98

FAKE_COMMIT_OVERRIDE=fedcba9876543210fedcba9876543210fedcba98 \
  expect_failure "build-tests accepted a commit that did not match checkout HEAD" \
  run_artifact build-tests "$tmpdir/out/wrong-source.tar.zst" "$commit"

FAKE_NEXTEST_VERSION_OVERRIDE=0.9.131 VERIFY_RUNTIME_OVERRIDE=1 \
  expect_failure "restore accepted the wrong nextest version" \
  run_artifact restore-tests "$bundle" "$tmpdir/wrong-nextest" "$commit"

FAKE_RUSTC_IDENTITY_OVERRIDE='rustc 1.96.0 (wrong)' VERIFY_RUNTIME_OVERRIDE=1 \
  expect_failure "restore accepted the wrong Rust toolchain" \
  run_artifact restore-tests "$bundle" "$tmpdir/wrong-rustc" "$commit"

RUSTFLAGS_OVERRIDE='-D warnings' VERIFY_RUNTIME_OVERRIDE=1 \
  expect_failure "restore accepted the wrong Rust flags" \
  run_artifact restore-tests "$bundle" "$tmpdir/wrong-flags" "$commit"

expect_failure "restore overwrote an existing destination" \
  run_artifact restore-tests "$bundle" "$tmpdir/restored" "$commit"
expect_failure "CLI restore overwrote an existing destination" \
  run_artifact restore-cli "$cli_bundle" "$tmpdir/restored-cli" "$commit"

MAX_BYTES_OVERRIDE=1 \
  expect_failure "restore accepted an over-budget bundle" \
  run_artifact restore-tests "$bundle" "$tmpdir/over-budget" "$commit"

mkdir "$tmpdir/tampered"
tar --zstd -xf "$bundle" -C "$tmpdir/tampered"
printf '\nchanged=true\n' >> "$tmpdir/tampered/manifest"
tar --zstd -cf "$tmpdir/out/altered-manifest.tar.zst" -C "$tmpdir/tampered" \
  harn-tests.tar.zst harn manifest SHA256SUMS
expect_failure "restore accepted an altered manifest" \
  run_artifact restore-tests "$tmpdir/out/altered-manifest.tar.zst" "$tmpdir/altered-manifest" "$commit"

mkdir "$tmpdir/tampered-cli"
tar --zstd -xf "$cli_bundle" -C "$tmpdir/tampered-cli"
printf 'corrupt\n' >> "$tmpdir/tampered-cli/harn"
tar --zstd -cf "$tmpdir/out/corrupt-cli.tar.zst" -C "$tmpdir/tampered-cli" \
  harn manifest CLI_SHA256SUMS
expect_failure "CLI restore accepted corrupt harn bytes" \
  run_artifact restore-cli "$tmpdir/out/corrupt-cli.tar.zst" "$tmpdir/corrupt-cli" "$commit"

printf 'corrupt\n' >> "$tmpdir/tampered/harn-tests.tar.zst"
tar --zstd -cf "$tmpdir/out/corrupt-member.tar.zst" -C "$tmpdir/tampered" \
  harn-tests.tar.zst harn manifest SHA256SUMS
expect_failure "restore accepted corrupt archive bytes" \
  run_artifact restore-tests "$tmpdir/out/corrupt-member.tar.zst" "$tmpdir/corrupt-member" "$commit"

tar --zstd -cf "$tmpdir/out/missing-member.tar.zst" -C "$tmpdir/tampered" \
  harn manifest SHA256SUMS
expect_failure "restore accepted a missing archive member" \
  run_artifact restore-tests "$tmpdir/out/missing-member.tar.zst" "$tmpdir/missing-member" "$commit"

cp "$tmpdir/work/rust-toolchain.toml" "$tmpdir/work/rust-toolchain.toml.saved"
printf '[toolchain]\nchannel = "1.96.0"\n' > "$tmpdir/work/rust-toolchain.toml"
expect_failure "restore accepted a different pinned toolchain" \
  run_artifact restore-tests "$bundle" "$tmpdir/wrong-toolchain-file" "$commit"
mv "$tmpdir/work/rust-toolchain.toml.saved" "$tmpdir/work/rust-toolchain.toml"

MAX_BYTES_OVERRIDE=1 \
  expect_failure "build-tests accepted an over-budget bundle" \
  run_artifact build-tests "$tmpdir/out/build-over-budget.tar.zst" "$commit"

echo "rust_artifact_test: ok"
