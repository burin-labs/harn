#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/rust_artifact.sh"
policy_nextest="$(jq -er '.nextest_version' "$repo_root/.github/cache-policy.json")"
expected_security_filter="$(sed -n "s/^readonly SECURITY_FILTER='\(.*\)'$/\1/p" "$script")"
expected_host_bound_filter="$("$repo_root/scripts/ci/host_bound_rust_test_filter.sh")"
[[ -n "$expected_security_filter" ]]
[[ "$expected_security_filter" == *'package(harn-cli) and binary(harn_cli_e2e)'* ]]
grep -Fxq 'canonical_fixture_scrubs_ambient_loader_controls_without_scrubbing_explicit_controls' \
  "$repo_root/scripts/config/host-bound-rust-tests.txt"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p "$tmpdir/bin" "$tmpdir/target/debug" "$tmpdir/target/ci-cli" "$tmpdir/out" "$tmpdir/receipts" "$tmpdir/work"
cp "$repo_root/rust-toolchain.toml" "$tmpdir/work/rust-toolchain.toml"
make_fake_security_inventory() {
  local omitted_test=${1:-}
  local extra_test=${2:-}
  jq -n --rawfile registry "$repo_root/scripts/config/host-bound-rust-tests.txt" \
    --arg omitted_test "$omitted_test" --arg extra_test "$extra_test" '
    ($registry | split("\n") | map(select(length > 0 and . != $omitted_test))
      + [$extra_test] | map(select(length > 0))) as $tests
    | {"test-count": ($tests | length), "rust-suites": {
        "fake": {
          "status": "listed",
          "package-name": "harn-cli",
          "binary-name": "harn_cli_e2e",
          "testcases": (reduce $tests[] as $name ({};
            .[$name] = {"filter-match": {"status": "matches"}}
          ))
        },
        "fake-skipped": {
          "status": "skipped",
          "package-name": "harn-vm",
          "binary-name": "unselected-suite",
          "testcases": {}
        }
      }}
  '
}
make_fake_security_inventory > "$tmpdir/security-inventory.json"
cat > "$tmpdir/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${CARGO_RECEIPTS:?}/cargo-calls"
case "$1" in
  build)
    # The shared CLI bundle builds in the ci-cli profile; the test bundle's
    # CLI stays in dev with the test build.
    case "$*" in
      "build --locked --bin harn") : > "${CARGO_RECEIPTS:?}/build" ;;
      "build --locked --profile ci-cli --bin harn") : > "${CARGO_RECEIPTS:?}/build-ci-cli" ;;
      *) exit 2 ;;
    esac
    ;;
  metadata)
    [[ "$*" == "metadata --format-version 1 --no-deps" ]] || exit 2
    printf '{"target_directory":"%s"}\n' "${FAKE_TARGET:?}"
    ;;
  nextest)
    if [[ "$#" -eq 2 && "$2" == "--version" ]]; then
      printf 'cargo-nextest %s (fake)\n' "${FAKE_NEXTEST_VERSION:-}"
      exit 0
    fi
    if [[ "$#" -eq 10 && "$2" == "list" && "$3" == "--profile" && \
      "$4" == "ci" && "$5" == "--archive-file" && -n "$6" && \
      "$7" == "--message-format" && "$8" == "json" && "$9" == "-E" && \
      "${10}" == "${EXPECTED_HOST_BOUND_FILTER:?}" ]]; then
      cat "${FAKE_NEXTTEST_INVENTORY:?}"
      : > "${CARGO_RECEIPTS:?}/nextest-security-list"
    elif [[ "$#" -eq 10 && "$2" == "archive" && "$3" == "--locked" && \
      "$4" == "--workspace" && "$5" == "--profile" && "$6" == "ci" && \
      "$7" == "-E" && "$8" == 'all()' && "$9" == "--archive-file" && -n "${10}" ]]; then
      printf 'tests archive\n' > "${10}"
      : > "${CARGO_RECEIPTS:?}/nextest-tests"
    elif [[ "$#" -eq 10 && "$2" == "archive" && "$3" == "--locked" && \
      "$4" == "--workspace" && "$5" == "--profile" && "$6" == "ci" && "$7" == "-E" && \
      "$8" == "${EXPECTED_SECURITY_FILTER:?}" && \
      "$9" == "--archive-file" && -n "${10}" ]]; then
      printf 'security tests archive\n' > "${10}"
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
printf '#!/usr/bin/env bash\necho harn\n' > "$tmpdir/target/ci-cli/harn"
chmod +x "$tmpdir/target/ci-cli/harn"

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
      EXPECTED_SECURITY_FILTER="$expected_security_filter" \
      EXPECTED_HOST_BOUND_FILTER="$expected_host_bound_filter" \
      FAKE_NEXTTEST_INVENTORY="${NEXTTEST_INVENTORY_OVERRIDE:-$tmpdir/security-inventory.json}" \
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
run_artifact build-cli "$cli_bundle" "$commit"
test -f "$tmpdir/receipts/build-ci-cli"
test ! -f "$tmpdir/receipts/build"
test ! -f "$tmpdir/receipts/nextest-tests"
run_artifact build-security "$security_bundle" "$commit"
test -f "$tmpdir/receipts/nextest-security"
test -f "$tmpdir/receipts/nextest-security-list"

# Missing one registered case in the archived inventory must fail publication.
fixture_test="$(grep '^canonical_fixture_' "$repo_root/scripts/config/host-bound-rust-tests.txt")"
make_fake_security_inventory "$fixture_test" > "$tmpdir/security-inventory-missing-fixture.json"
if NEXTTEST_INVENTORY_OVERRIDE="$tmpdir/security-inventory-missing-fixture.json" \
  run_artifact build-security "$tmpdir/out/security-missing-fixture.tar.zst" "$commit" \
  > "$tmpdir/security-missing-fixture.out" 2>&1; then
  echo "build-security published an archive missing the registered CLI fixture" >&2
  exit 1
fi
grep -Fq "host-bound registry entry is absent from archived tests: $fixture_test" \
  "$tmpdir/security-missing-fixture.out"
test ! -e "$tmpdir/out/security-missing-fixture.tar.zst"

# A selected test outside the canonical registry must also block publication.
make_fake_security_inventory "" unexpected_probe > "$tmpdir/security-inventory-extra.json"
if NEXTTEST_INVENTORY_OVERRIDE="$tmpdir/security-inventory-extra.json" \
  run_artifact build-security "$tmpdir/out/security-extra-test.tar.zst" "$commit" \
  > "$tmpdir/security-extra-test.out" 2>&1; then
  echo "build-security accepted a selected test outside the host-bound registry" >&2
  exit 1
fi
grep -Fq 'archived filter selected a test outside the host-bound registry' \
  "$tmpdir/security-extra-test.out"
test ! -e "$tmpdir/out/security-extra-test.tar.zst"

# A status outside nextest's known listed/skipped vocabulary is malformed.
jq '."rust-suites"."fake-skipped".status = "unreported"' \
  "$tmpdir/security-inventory.json" > "$tmpdir/security-inventory-unknown-status.json"
if NEXTTEST_INVENTORY_OVERRIDE="$tmpdir/security-inventory-unknown-status.json" \
  run_artifact build-security "$tmpdir/out/security-unknown-status.tar.zst" "$commit" \
  > "$tmpdir/security-unknown-status.out" 2>&1; then
  echo "build-security accepted an unknown nextest suite status" >&2
  exit 1
fi
grep -Fq 'nextest archive inventory contains invalid or incomplete Rust suites' \
  "$tmpdir/security-unknown-status.out"
test ! -e "$tmpdir/out/security-unknown-status.tar.zst"

# A broken host-bound registry must fail before the consumer can invoke
# nextest, rather than reporting a successful proof with no selected tests.
bad_filter_repo="$tmpdir/bad-filter-repo"
mkdir -p "$bad_filter_repo/scripts/ci" "$bad_filter_repo/scripts/config" \
  "$bad_filter_repo/scripts/lib" "$bad_filter_repo/.github" "$tmpdir/bad-filter-receipts"
cp "$repo_root/scripts/ci/rust_artifact.sh" \
  "$repo_root/scripts/ci/host_bound_rust_test_filter.sh" \
  "$bad_filter_repo/scripts/ci/"
cp "$repo_root/scripts/ci/cache_policy.sh" "$bad_filter_repo/scripts/ci/"
cp "$repo_root/scripts/lib/sha256.sh" "$bad_filter_repo/scripts/lib/"
cp "$repo_root/.github/cache-policy.json" "$bad_filter_repo/.github/"
cp "$repo_root/rust-toolchain.toml" "$bad_filter_repo/"
: > "$bad_filter_repo/scripts/config/host-bound-rust-tests.txt"
cat > "$tmpdir/bad-filter-consumer.sh" <<SH
#!/usr/bin/env bash
set -euo pipefail
host_bound_filter=\$("$bad_filter_repo/scripts/ci/host_bound_rust_test_filter.sh")
cargo nextest run -E "\$host_bound_filter"
SH
chmod +x "$tmpdir/bad-filter-consumer.sh"
if (
  env PATH="$tmpdir/bin:$PATH" \
    CARGO_RECEIPTS="$tmpdir/bad-filter-receipts" \
    "$tmpdir/bad-filter-consumer.sh"
) > "$tmpdir/bad-filter.out" 2>&1; then
  echo "Linux sandbox consumer accepted an empty host-bound test selector" >&2
  exit 1
fi
grep -Fxq 'host-bound Rust test list is empty' "$tmpdir/bad-filter.out"
test ! -e "$tmpdir/bad-filter-receipts/cargo-calls"

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

# The main-branch cache warmer must build the profile the producer reads, or a
# cache hit still recompiles the whole CLI.
shared_profile="$(sed -n "s/^readonly SHARED_CLI_PROFILE='\(.*\)'$/\1/p" "$script")"
[[ -n "$shared_profile" ]]
grep -Fxq "cargo build --locked --profile ${shared_profile} --bin harn" \
  "$repo_root/scripts/ci/warm_harn_cli_cache.sh" || {
  echo "warm_harn_cli_cache.sh does not warm the ${shared_profile} profile build-cli reads" >&2
  exit 1
}

# A shared CLI from any other profile is refused even when its checksums are
# consistent: the proof lanes' timing budgets assume the optimized build.
grep -Fxq 'cargo_profile=ci-cli' "$tmpdir/restored-cli/manifest"
mkdir "$tmpdir/dev-profile-cli"
tar --zstd -xf "$cli_bundle" -C "$tmpdir/dev-profile-cli"
sed -i.bak 's/^cargo_profile=ci-cli$/cargo_profile=dev/' "$tmpdir/dev-profile-cli/manifest"
rm "$tmpdir/dev-profile-cli/manifest.bak"
(
  cd "$tmpdir/dev-profile-cli"
  sha256sum harn manifest > CLI_SHA256SUMS
)
tar --zstd -cf "$tmpdir/out/dev-profile-cli.tar.zst" -C "$tmpdir/dev-profile-cli" \
  harn manifest CLI_SHA256SUMS
expect_failure "CLI restore accepted a bundle built outside the ci-cli profile" \
  run_artifact restore-cli "$tmpdir/out/dev-profile-cli.tar.zst" "$tmpdir/dev-profile-restored" "$commit"

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
candidate_file="harn-${candidate_target}.tar.gz"
mkdir -p "$tmpdir/candidate-src" "$tmpdir/candidate"
printf '#!/usr/bin/env bash\necho candidate-harn\n' > "$tmpdir/candidate-src/harn"
chmod +x "$tmpdir/candidate-src/harn"
ln -s harn "$tmpdir/candidate-src/harn-lsp"
tar -czf "$tmpdir/candidate/$candidate_file" -C "$tmpdir/candidate-src" harn harn-lsp
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

echo "rust_artifact_test: ok"
