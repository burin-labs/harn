#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/rust_artifact.sh"
policy_nextest="$(jq -er '.nextest_version' "$repo_root/.github/cache-policy.json")"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p "$tmpdir/bin" "$tmpdir/target/debug" "$tmpdir/target/ci-cli" "$tmpdir/out" "$tmpdir/receipts" "$tmpdir/work"
cp "$repo_root/rust-toolchain.toml" "$tmpdir/work/rust-toolchain.toml"
cat > "$tmpdir/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  build)
    # The shared CLI bundle builds in the ci-cli profile; the test bundle's
    # CLI stays in dev with the test build.
    case "$*" in
      "build --locked --bin harn") : > "${CARGO_RECEIPTS:?}/build" ;;
      "build --locked --profile ci-cli -p harn-cli -p harn-cli-aot-gen --bin harn --bin harn-cli-aot-gen") : > "${CARGO_RECEIPTS:?}/build-ci-cli" ;;
      *) exit 2 ;;
    esac
    if [[ "${FAKE_HEAD_CLI:-0}" == 1 ]]; then
      printf '#!/usr/bin/env bash\nprintf '\''{"data":{"source_revision":"%s"}}\\n'\''\n' \
        "${FAKE_EMBEDDED_REVISION:-${HARN_BUILD_REVISION:?}}" > "${FAKE_TARGET:?}/ci-cli/harn"
      chmod +x "${FAKE_TARGET:?}/ci-cli/harn"
    fi
    ;;
  metadata)
    if [[ "$*" == "metadata --locked --format-version 1" ]]; then
      printf '%s\n' '{"workspace_members":["vm","hostlib"],"packages":[{"name":"harn-vm","source":null,"id":"vm"},{"name":"harn-hostlib","source":null,"id":"hostlib"}],"resolve":{"nodes":[{"id":"vm","features":["default","otel"]},{"id":"hostlib","features":["full"]}]}}'
      exit 0
    fi
    [[ "$*" == "metadata --format-version 1 --no-deps" ]] || exit 2
    printf '{"target_directory":"%s"}\n' "${FAKE_TARGET:?}"
    ;;
  nextest)
    # Cargo's scoped bin directory wins over PATH. A successful CI fixture
    # must bypass this stale subcommand, just like the installed runner.
    printf 'cargo-nextest 0.9.146 (stale scoped Cargo binary)\n'
    exit 0
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$tmpdir/bin/cargo"
cat > "$tmpdir/bin/cargo-nextest" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  nextest)
    if [[ "$#" -eq 2 && "$2" == "--version" ]]; then
      printf 'cargo-nextest %s (fake)\n' "${FAKE_NEXTEST_VERSION:-}"
      exit 0
    fi
    if [[ "$#" -eq 8 && "$2" == "run" && "$3" == "--locked" && \
      "$4" == "--workspace" && "$5" == "--profile" && "$6" == "ci" && \
      "$7" == "-E" && "$8" == 'test(open_enum_source_compatibility)' ]]; then
      [[ "${HARN_ENUM_COMPILER_ARCHIVE_STAGE:-}" == 1 ]] || exit 2
      : > "${CARGO_RECEIPTS:?}/compiler-fixture-controls"
      [[ "${FAKE_COMPILER_FIXTURE_FAILURE:-0}" == 0 ]] || exit 1
    elif [[ "$#" -eq 12 && "$2" == "archive" && "$3" == "--locked" && \
      "$4" == "--workspace" && "$5" == "--profile" && "$6" == "ci" && \
      "$7" == "--tool-config-file" && "$8" == harn-compiler-archive:*/.config/nextest-compiler-archive.toml && \
      "$9" == "-E" && "${10}" == 'all()' && "${11}" == "--archive-file" && -n "${12}" ]]; then
      [[ -f "${CARGO_RECEIPTS:?}/compiler-fixture-controls" ]] || exit 2
      printf 'tests archive\n' > "${12}"
      : > "${CARGO_RECEIPTS:?}/nextest-tests"
    elif [[ "$#" -eq 15 && "$2" == "archive" && "$3" == "--locked" && \
      "$4" == "-p" && "$5" == "harn-vm" && "$6" == "-p" && "$7" == "harn-hostlib" && \
      "$8" == "--features" && "$9" == 'harn-hostlib/full,harn-vm/default,harn-vm/otel' && \
      "${10}" == "--profile" && "${11}" == "ci" && "${12}" == "-E" && \
      "${13}" == '(package(harn-vm) and binary(harn_vm)) or (package(harn-hostlib) and binary(harn_hostlib))' && \
      "${14}" == "--archive-file" && -n "${15}" ]]; then
      printf 'security tests archive\n' > "${15}"
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
chmod +x "$tmpdir/bin/cargo-nextest"
cat > "$tmpdir/bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "rev-parse --verify HEAD" ]]; then
  printf '%s\n' "${FAKE_COMMIT:?}"
  exit 0
fi
case "$*" in
  "cat-file -e "*"^{commit}"|"diff --quiet HEAD --"|"ls-files --others --exclude-standard")
    [[ "$1" != diff || "${FAKE_DIRTY_SOURCE:-0}" == 0 ]] || exit 1
    exit 0 ;;
  "rev-parse --verify HEAD^{tree}") printf '%s\n' same-tree; exit 0 ;;
  "rev-parse --verify "*"^{tree}") printf '%s\n' "${FAKE_SOURCE_TREE:-same-tree}"; exit 0 ;;
esac
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
printf '#!/usr/bin/env bash\necho harn\n' > "$tmpdir/target/ci-cli/harn"
chmod +x "$tmpdir/target/ci-cli/harn"
printf '#!/usr/bin/env bash\necho aot-generator\n' > "$tmpdir/target/ci-cli/harn-cli-aot-gen"
chmod +x "$tmpdir/target/ci-cli/harn-cli-aot-gen"

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
      FAKE_NEXTEST_VERSION="${FAKE_NEXTEST_VERSION_OVERRIDE:-$policy_nextest}" \
      FAKE_RUSTC_IDENTITY="${FAKE_RUSTC_IDENTITY_OVERRIDE:-rustc 1.95.0 (fake)}" \
      RUSTFLAGS="${RUSTFLAGS_OVERRIDE:--D warnings -Clink-arg=-fuse-ld=mold}" \
      CARGO_PROFILE_DEV_DEBUG="${DEV_DEBUG_OVERRIDE:-}" \
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
test -f "$tmpdir/receipts/compiler-fixture-controls"
rm -f "$tmpdir/receipts/build" "$tmpdir/receipts/nextest-tests"
run_artifact build-cli "$cli_bundle" "$commit"
test -f "$tmpdir/receipts/build-ci-cli"
test ! -f "$tmpdir/receipts/build"
test ! -f "$tmpdir/receipts/nextest-tests"
head_commit=fedcba9876543210fedcba9876543210fedcba98
FAKE_HEAD_CLI=1 run_artifact build-head-cli "$tmpdir/out/head-cli.tar.zst" "$head_commit"
mkdir "$tmpdir/head-cli"
tar --zstd -xf "$tmpdir/out/head-cli.tar.zst" -C "$tmpdir/head-cli"
grep -Fxq "commit=$head_commit" "$tmpdir/head-cli/manifest"
test "$("$tmpdir/head-cli/harn" version --json | jq -er '.data.source_revision')" = "$head_commit"
FAKE_HEAD_CLI=1 FAKE_SOURCE_TREE=different-tree \
  expect_failure "exact-head CLI accepted a different source tree" \
  run_artifact build-head-cli "$tmpdir/out/wrong-head-tree.tar.zst" "$head_commit"
FAKE_HEAD_CLI=1 FAKE_DIRTY_SOURCE=1 \
  expect_failure "exact-head CLI accepted dirty source" \
  run_artifact build-head-cli "$tmpdir/out/dirty-head-tree.tar.zst" "$head_commit"
FAKE_HEAD_CLI=1 FAKE_EMBEDDED_REVISION="$commit" \
  expect_failure "exact-head CLI accepted a stale embedded source revision" \
  run_artifact build-head-cli "$tmpdir/out/stale-head-cli.tar.zst" "$head_commit"
run_artifact build-security "$security_bundle" "$commit"
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
grep -Fxq "HARN_RELEASE_CLI_AOT_GEN_BIN=$restored_cli/harn-cli-aot-gen" "$cli_github_env"
"$restored_cli/harn-cli-aot-gen" | grep -Fxq aot-generator
tar --zstd -tf "$cli_bundle" | sort | diff -u - <(printf '%s\n' \
  CLI_SHA256SUMS harn harn-cli-aot-gen manifest | sort)

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

# The producer publishes the split CLI and security bundles independently.
FAKE_COMMIT_OVERRIDE=fedcba9876543210fedcba9876543210fedcba98 \
  expect_failure "build-cli accepted a commit that did not match checkout HEAD" \
  run_artifact build-cli "$tmpdir/out/cli-only-wrong.tar.zst" "$commit"
FAKE_COMMIT_OVERRIDE=fedcba9876543210fedcba9876543210fedcba98 \
  expect_failure "build-security accepted a commit that did not match checkout HEAD" \
  run_artifact build-security "$tmpdir/out/split-security-wrong.tar.zst" "$commit"

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

DEV_DEBUG_OVERRIDE=full \
  expect_failure "build-tests accepted a dev debuginfo override that disagrees with Cargo.toml" \
  run_artifact build-tests "$tmpdir/out/wrong-dev-debug.tar.zst" "$commit"

# The artifact manifest records the dev debuginfo the script expects; that
# value must be the one `[profile.dev]` in Cargo.toml actually builds with.
manifest_dev_debug="$(awk '/^\[profile\.dev\]$/ { p = 1; next } /^\[/ { p = 0 } p && $1 == "debug" { gsub(/"/, "", $3); print $3 }' "$repo_root/Cargo.toml")"
script_dev_debug="$(sed -n "s/^readonly EXPECTED_DEV_DEBUG='\(.*\)'$/\1/p" "$script")"
if [[ -z "$manifest_dev_debug" || "$manifest_dev_debug" != "$script_dev_debug" ]]; then
  echo "rust_artifact.sh EXPECTED_DEV_DEBUG ('$script_dev_debug') does not match Cargo.toml [profile.dev] debug ('$manifest_dev_debug')" >&2
  exit 1
fi
workflow_dev_debug="$(awk '/^env:$/ { p = 1; next } /^jobs:$/ { p = 0 } p && $1 == "CARGO_PROFILE_DEV_DEBUG:" { print $2 }' "$repo_root/.github/workflows/ci.yml")"
if [[ -n "$workflow_dev_debug" ]]; then
  echo "CI must use Cargo.toml [profile.dev] debug without a redundant cache-key environment override" >&2
  exit 1
fi

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
  harn harn-cli-aot-gen manifest CLI_SHA256SUMS
expect_failure "CLI restore accepted corrupt harn bytes" \
  run_artifact restore-cli "$tmpdir/out/corrupt-cli.tar.zst" "$tmpdir/corrupt-cli" "$commit"

# A shared CLI from any other profile is refused even when its checksums are
# consistent: the proof lanes' timing budgets assume the optimized build.
grep -Fxq 'cargo_profile=ci-cli' "$tmpdir/restored-cli/manifest"
mkdir "$tmpdir/dev-profile-cli"
tar --zstd -xf "$cli_bundle" -C "$tmpdir/dev-profile-cli"
sed -i.bak 's/^cargo_profile=ci-cli$/cargo_profile=dev/' "$tmpdir/dev-profile-cli/manifest"
rm "$tmpdir/dev-profile-cli/manifest.bak"
(
  cd "$tmpdir/dev-profile-cli"
  sha256sum harn harn-cli-aot-gen manifest > CLI_SHA256SUMS
)
tar --zstd -cf "$tmpdir/out/dev-profile-cli.tar.zst" -C "$tmpdir/dev-profile-cli" \
  harn harn-cli-aot-gen manifest CLI_SHA256SUMS
expect_failure "CLI restore accepted a bundle built outside the ci-cli profile" \
  run_artifact restore-cli "$tmpdir/out/dev-profile-cli.tar.zst" "$tmpdir/dev-profile-restored" "$commit"

# A valid outer checksum cannot authorize a generator different from the one
# named in the exact-commit tool manifest.
mkdir "$tmpdir/tampered-generator"
tar --zstd -xf "$cli_bundle" -C "$tmpdir/tampered-generator"
printf 'changed\n' >> "$tmpdir/tampered-generator/harn-cli-aot-gen"
(
  cd "$tmpdir/tampered-generator"
  sha256sum harn harn-cli-aot-gen manifest > CLI_SHA256SUMS
)
tar --zstd -cf "$tmpdir/out/corrupt-generator.tar.zst" -C "$tmpdir/tampered-generator" \
  harn harn-cli-aot-gen manifest CLI_SHA256SUMS
expect_failure "CLI restore accepted a different AOT generator under the source manifest" \
  run_artifact restore-cli "$tmpdir/out/corrupt-generator.tar.zst" "$tmpdir/corrupt-generator" "$commit"

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

# restore-candidate: a release candidate archive, checked against the candidate
# manifest the build wrote at the same commit.
candidate_target=x86_64-unknown-linux-gnu
candidate_file="harn-${candidate_target}.tar.xz"
mkdir -p "$tmpdir/candidate-src" "$tmpdir/candidate"
printf '#!/usr/bin/env bash\necho candidate-harn\n' > "$tmpdir/candidate-src/harn"
chmod +x "$tmpdir/candidate-src/harn"
ln -s harn "$tmpdir/candidate-src/harn-lsp"
tar -cJf "$tmpdir/candidate/$candidate_file" -C "$tmpdir/candidate-src" harn harn-lsp
candidate_sha="$(sha256sum "$tmpdir/candidate/$candidate_file" | cut -d ' ' -f 1)"
write_candidate_manifest() {
  local output=$1 source_commit=$2 digest=$3
  jq -n --arg commit "$source_commit" --arg sha "$digest" \
    --arg target "$candidate_target" --arg file "$candidate_file" '{
      schemaVersion: "burin-labs.candidate_manifest.v1",
      repository: "burin-labs/harn",
      sourceCommit: $commit,
      runId: "1",
      runAttempt: "1",
      artifacts: [{
        kind: "archive", target: $target, artifact: ("harn-" + $target), file: $file,
        sha256: $sha,
        attestationPredicateType: "https://harnlang.com/attestations/release-archive/v1",
        signingStatus: "not_applicable", notarizationStatus: "not_applicable"
      }]
    }' > "$output"
}
write_candidate_manifest "$tmpdir/candidate/manifest.json" "$commit" "$candidate_sha"

candidate_env="$tmpdir/candidate-github-env"
run_artifact restore-candidate "$tmpdir/candidate/$candidate_file" \
  "$tmpdir/candidate/manifest.json" "$candidate_target" "$tmpdir/restored-candidate" \
  "$commit" "$candidate_env"
restored_candidate="$(cd "$tmpdir/restored-candidate" && pwd -P)"
"$restored_candidate/harn" | grep -Fxq candidate-harn
test -L "$restored_candidate/harn-lsp"
grep -Fxq "HARN_BIN=$restored_candidate/harn" "$candidate_env"
grep -Fxq "SOURCE_GATE_CI_BINARY_COMMIT=$commit" "$candidate_env"
grep -Fxq "SOURCE_GATE_CI_BINARY_SHA256=$(sha256sum "$restored_candidate/harn" | cut -d ' ' -f 1)" \
  "$candidate_env"
grep -Fxq "SOURCE_GATE_CI_BINARY_BUILD_FRESHNESS_ID=$candidate_sha" "$candidate_env"

# Negative control: the checkout is not the commit the caller asked for.
FAKE_COMMIT_OVERRIDE=fedcba9876543210fedcba9876543210fedcba98 \
  expect_failure "restore-candidate accepted a checkout at another commit" \
  run_artifact restore-candidate "$tmpdir/candidate/$candidate_file" \
  "$tmpdir/candidate/manifest.json" "$candidate_target" "$tmpdir/candidate-wrong-head" "$commit"
test ! -e "$tmpdir/candidate-wrong-head"

# Negative control: the archive's bytes are not the bytes the manifest records.
cp "$tmpdir/candidate/$candidate_file" "$tmpdir/candidate/substituted.tar.gz"
printf 'substituted\n' >> "$tmpdir/candidate/substituted.tar.gz"
mkdir -p "$tmpdir/candidate-substituted"
mv "$tmpdir/candidate/substituted.tar.gz" "$tmpdir/candidate-substituted/$candidate_file"
expect_failure "restore-candidate accepted an archive whose digest differs from the manifest" \
  run_artifact restore-candidate "$tmpdir/candidate-substituted/$candidate_file" \
  "$tmpdir/candidate/manifest.json" "$candidate_target" "$tmpdir/candidate-bad-digest" "$commit"
test ! -e "$tmpdir/candidate-bad-digest"

# Negative control: a manifest another commit's build wrote, even with matching bytes.
write_candidate_manifest "$tmpdir/candidate/other-commit.json" \
  fedcba9876543210fedcba9876543210fedcba98 "$candidate_sha"
expect_failure "restore-candidate accepted a manifest from another commit" \
  run_artifact restore-candidate "$tmpdir/candidate/$candidate_file" \
  "$tmpdir/candidate/other-commit.json" "$candidate_target" "$tmpdir/candidate-other-commit" "$commit"
test ! -e "$tmpdir/candidate-other-commit"

expect_failure "restore-candidate accepted a target the manifest does not list" \
  run_artifact restore-candidate "$tmpdir/candidate/$candidate_file" \
  "$tmpdir/candidate/manifest.json" aarch64-apple-darwin "$tmpdir/candidate-other-target" "$commit"
expect_failure "restore-candidate overwrote an existing destination" \
  run_artifact restore-candidate "$tmpdir/candidate/$candidate_file" \
  "$tmpdir/candidate/manifest.json" "$candidate_target" "$tmpdir/restored-candidate" "$commit"

# A green archive cannot hide a failed compiler fixture or publish stale inputs.
rm -f "$tmpdir/receipts/nextest-tests"
FAKE_COMPILER_FIXTURE_FAILURE=1 \
  expect_failure "build-tests accepted failed compiler fixture controls" \
  run_artifact build-tests "$tmpdir/out/failed-compiler-fixture.tar.zst" "$commit"
test ! -f "$tmpdir/receipts/nextest-tests"
test ! -e "$tmpdir/out/failed-compiler-fixture.tar.zst"

echo "rust_artifact_test: ok"
