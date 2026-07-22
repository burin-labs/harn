#!/usr/bin/env bash
set -euo pipefail

readonly NEXTEST_VERSION="0.9.132"
readonly NEUTRAL_FILTER='all()'
readonly SECURITY_FILTER='package(harn-vm) and binary(harn_vm)'
readonly EXPECTED_RUSTFLAGS='-D warnings -Clink-arg=-fuse-ld=mold'
readonly EXPECTED_DEV_DEBUG='line-tables-only'
readonly DEFAULT_MAX_BUNDLE_BYTES=9663676416  # 9 GiB: workspace nextest archive is ~8.4 GiB today
readonly DEFAULT_MAX_SECURITY_BUNDLE_BYTES=3221225472  # 3 GiB: single-package archive
cleanup_dir=""
trap '[[ -z "$cleanup_dir" ]] || rm -rf "$cleanup_dir"' EXIT

usage() {
  cat <<'EOF'
usage:
  scripts/ci/behavior_artifact.sh build <bundle.tar.zst> <cli-bundle.tar.zst> <security-bundle.tar.zst> <commit-sha>
  scripts/ci/behavior_artifact.sh restore <bundle.tar.zst> <directory> <commit-sha> [github-env]
  scripts/ci/behavior_artifact.sh restore-cli <cli-bundle.tar.zst> <directory> <commit-sha> [github-env]
  scripts/ci/behavior_artifact.sh restore-security <security-bundle.tar.zst> <directory> <commit-sha>

env:
  HARN_BEHAVIOR_SKIP_NEUTRAL_ARCHIVE=1  build: skip the workspace nextest
    archive and the neutral bundle (the co-located suite proves the workspace
    in place; only the CLI and security bundles are produced).
EOF
}

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

require_nextest_version() {
  local actual first_line program version
  actual="$(cargo nextest --version)"
  first_line="${actual%%$'\n'*}"
  read -r program version _ <<< "$first_line"
  if [[ "$program" != "cargo-nextest" || "$version" != "$NEXTEST_VERSION" ]]; then
    echo "error: expected cargo-nextest ${NEXTEST_VERSION}, got: ${actual}" >&2
    exit 1
  fi
}

validate_commit() {
  if [[ ! "$1" =~ ^[0-9a-f]{40}$ ]]; then
    echo "error: expected a lowercase 40-character commit SHA, got: $1" >&2
    exit 2
  fi
}

require_source_commit() {
  local expected=$1
  local actual
  actual="$(git rev-parse --verify HEAD)"
  if [[ "$actual" != "$expected" ]]; then
    echo "error: behavior artifact commit ${expected} does not match checkout HEAD ${actual}" >&2
    exit 1
  fi
}

max_bundle_bytes() {
  local value="${HARN_BEHAVIOR_ARTIFACT_MAX_BYTES:-$DEFAULT_MAX_BUNDLE_BYTES}"
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: HARN_BEHAVIOR_ARTIFACT_MAX_BYTES must be a positive integer" >&2
    exit 2
  fi
  printf '%s\n' "$value"
}

require_size_budget() {
  local path=$1
  local limit size
  limit="$(max_bundle_bytes)"
  size="$(wc -c < "$path" | tr -d ' ')"
  if (( size > limit )); then
    echo "error: behavior artifact is ${size} bytes; limit is ${limit} bytes" >&2
    exit 1
  fi
}

max_security_bundle_bytes() {
  local value="${HARN_SECURITY_ARTIFACT_MAX_BYTES:-$DEFAULT_MAX_SECURITY_BUNDLE_BYTES}"
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: HARN_SECURITY_ARTIFACT_MAX_BYTES must be a positive integer" >&2
    exit 2
  fi
  printf '%s\n' "$value"
}

require_security_size_budget() {
  local path=$1
  local limit size
  limit="$(max_security_bundle_bytes)"
  size="$(wc -c < "$path" | tr -d ' ')"
  if (( size > limit )); then
    echo "error: security artifact is ${size} bytes; limit is ${limit} bytes" >&2
    exit 1
  fi
}

require_build_contract() {
  if [[ "${RUSTFLAGS:-}" != "$EXPECTED_RUSTFLAGS" ]]; then
    echo "error: behavior artifact requires RUSTFLAGS=${EXPECTED_RUSTFLAGS}" >&2
    exit 1
  fi
  if [[ "${CARGO_PROFILE_DEV_DEBUG:-}" != "$EXPECTED_DEV_DEBUG" ]]; then
    echo "error: behavior artifact requires CARGO_PROFILE_DEV_DEBUG=${EXPECTED_DEV_DEBUG}" >&2
    exit 1
  fi
}

rustc_identity_sha256() {
  rustc -vV | sha256sum | cut -d ' ' -f 1
}

write_manifest() {
  local destination=$1
  local commit=$2
  local rustc_digest=$3
  cat > "$destination/manifest" <<EOF
schema=harn.behavior_artifact.v3
commit=${commit}
nextest=${NEXTEST_VERSION}
rustc_sha256=${rustc_digest}
rust_toolchain_sha256=$(sha256 rust-toolchain.toml)
rustflags_sha256=$(printf '%s' "$EXPECTED_RUSTFLAGS" | sha256sum | cut -d ' ' -f 1)
dev_debug_sha256=$(printf '%s' "$EXPECTED_DEV_DEBUG" | sha256sum | cut -d ' ' -f 1)
neutral_filter_sha256=$(printf '%s' "$NEUTRAL_FILTER" | sha256sum | cut -d ' ' -f 1)
security_filter_sha256=$(printf '%s' "$SECURITY_FILTER" | sha256sum | cut -d ' ' -f 1)
harn_sha256=$(sha256 "$destination/harn")
EOF
  # The neutral tests archive is absent when the co-located suite proves the
  # workspace in place; the CLI-bundle consumers never check this key.
  if [[ -f "$destination/harn-tests.tar.zst" ]]; then
    echo "tests_archive_sha256=$(sha256 "$destination/harn-tests.tar.zst")" >> "$destination/manifest"
  fi
}

write_security_manifest() {
  local destination=$1
  local commit=$2
  local rustc_digest=$3
  cat > "$destination/manifest" <<EOF
schema=harn.behavior_artifact.security.v1
commit=${commit}
nextest=${NEXTEST_VERSION}
rustc_sha256=${rustc_digest}
rust_toolchain_sha256=$(sha256 rust-toolchain.toml)
rustflags_sha256=$(printf '%s' "$EXPECTED_RUSTFLAGS" | sha256sum | cut -d ' ' -f 1)
dev_debug_sha256=$(printf '%s' "$EXPECTED_DEV_DEBUG" | sha256sum | cut -d ' ' -f 1)
security_filter_sha256=$(printf '%s' "$SECURITY_FILTER" | sha256sum | cut -d ' ' -f 1)
tests_archive_sha256=$(sha256 "$destination/harn-security-tests.tar.zst")
EOF
}

require_manifest_value() {
  local manifest=$1
  local key=$2
  local expected=$3
  local actual
  actual="$(sed -n "s/^${key}=//p" "$manifest")"
  if [[ -z "$actual" || "$actual" != "$expected" ]]; then
    echo "error: behavior artifact manifest ${key} does not match this CI execution" >&2
    exit 1
  fi
}

verify_manifest() {
  local destination=$1
  local commit=$2
  require_manifest_value "$destination/manifest" schema harn.behavior_artifact.v3
  require_manifest_value "$destination/manifest" commit "$commit"
  require_manifest_value "$destination/manifest" nextest "$NEXTEST_VERSION"
  require_manifest_value "$destination/manifest" rust_toolchain_sha256 "$(sha256 rust-toolchain.toml)"
  require_manifest_value "$destination/manifest" rustflags_sha256 "$(printf '%s' "$EXPECTED_RUSTFLAGS" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" dev_debug_sha256 "$(printf '%s' "$EXPECTED_DEV_DEBUG" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" neutral_filter_sha256 "$(printf '%s' "$NEUTRAL_FILTER" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" security_filter_sha256 "$(printf '%s' "$SECURITY_FILTER" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" harn_sha256 "$(sha256 "$destination/harn")"
  require_manifest_value "$destination/manifest" tests_archive_sha256 "$(sha256 "$destination/harn-tests.tar.zst")"

  if [[ "${HARN_VERIFY_RUST_RUNTIME:-0}" == "1" ]]; then
    require_nextest_version
    require_build_contract
    require_manifest_value "$destination/manifest" rustc_sha256 "$(rustc_identity_sha256)"
  fi
}

verify_security_manifest() {
  local destination=$1
  local commit=$2
  require_manifest_value "$destination/manifest" schema harn.behavior_artifact.security.v1
  require_manifest_value "$destination/manifest" commit "$commit"
  require_manifest_value "$destination/manifest" nextest "$NEXTEST_VERSION"
  require_manifest_value "$destination/manifest" rust_toolchain_sha256 "$(sha256 rust-toolchain.toml)"
  require_manifest_value "$destination/manifest" rustflags_sha256 "$(printf '%s' "$EXPECTED_RUSTFLAGS" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" dev_debug_sha256 "$(printf '%s' "$EXPECTED_DEV_DEBUG" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" security_filter_sha256 "$(printf '%s' "$SECURITY_FILTER" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" tests_archive_sha256 "$(sha256 "$destination/harn-security-tests.tar.zst")"

  if [[ "${HARN_VERIFY_RUST_RUNTIME:-0}" == "1" ]]; then
    require_nextest_version
    require_build_contract
    require_manifest_value "$destination/manifest" rustc_sha256 "$(rustc_identity_sha256)"
  fi
}

verify_cli_manifest() {
  local destination=$1
  local commit=$2
  require_manifest_value "$destination/manifest" schema harn.behavior_artifact.v3
  require_manifest_value "$destination/manifest" commit "$commit"
  require_manifest_value "$destination/manifest" rust_toolchain_sha256 "$(sha256 rust-toolchain.toml)"
  require_manifest_value "$destination/manifest" rustflags_sha256 "$(printf '%s' "$EXPECTED_RUSTFLAGS" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" dev_debug_sha256 "$(printf '%s' "$EXPECTED_DEV_DEBUG" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" harn_sha256 "$(sha256 "$destination/harn")"
}

report_timing() {
  local operation=$1
  local seconds=$2
  local bytes=$3
  echo "behavior_artifact_${operation}_seconds=${seconds}"
  echo "behavior_artifact_bytes=${bytes}"
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "### Behavior artifact ${operation}"
      echo
      echo "- Duration: ${seconds}s"
      echo "- Compressed bytes: ${bytes}"
    } >> "$GITHUB_STEP_SUMMARY"
  fi
}

build_bundle() {
  local output=$1
  local cli_output=$2
  local security_output=$3
  local commit=$4
  local output_dir cli_output_dir security_output_dir target_dir staging started bytes
  local skip_neutral="${HARN_BEHAVIOR_SKIP_NEUTRAL_ARCHIVE:-0}"
  started=$SECONDS

  validate_commit "$commit"
  require_source_commit "$commit"
  require_nextest_version
  require_build_contract
  mkdir -p "$(dirname "$output")"
  output_dir="$(cd "$(dirname "$output")" && pwd -P)"
  output="${output_dir}/$(basename "$output")"
  mkdir -p "$(dirname "$cli_output")"
  cli_output_dir="$(cd "$(dirname "$cli_output")" && pwd -P)"
  cli_output="${cli_output_dir}/$(basename "$cli_output")"
  mkdir -p "$(dirname "$security_output")"
  security_output_dir="$(cd "$(dirname "$security_output")" && pwd -P)"
  security_output="${security_output_dir}/$(basename "$security_output")"
  target_dir="$(cargo metadata --format-version 1 --no-deps | jq -er '.target_directory')"
  staging="$(mktemp -d "${output_dir}/behavior-artifact.XXXXXX")"
  cleanup_dir="$staging"
  mkdir -p "$staging/security"

  cargo build --locked --bin harn
  # One archive covers the fallback consumer lane. Skipped when the co-located
  # suite executes the compiled workspace in place — compressing and shipping
  # ~8.4 GiB of test binaries proves nothing the warm workspace run does not.
  if [[ "$skip_neutral" != "1" ]]; then
    cargo nextest archive --locked --workspace --profile ci \
      -E "$NEUTRAL_FILTER" \
      --archive-file "$staging/harn-tests.tar.zst"
  fi
  # The security lane executes one Landlock proof from harn-vm; ship it a
  # single-package archive instead of the multi-gigabyte workspace bundle.
  cargo nextest archive --locked -p harn-vm --profile ci \
    --archive-file "$staging/security/harn-security-tests.tar.zst"

  if [[ ! -x "$target_dir/debug/harn" ]]; then
    echo "error: build did not produce the required harn CLI at $target_dir/debug/harn" >&2
    exit 1
  fi
  install -m 0755 "$target_dir/debug/harn" "$staging/harn"
  write_manifest "$staging" "$commit" "$(rustc_identity_sha256)"
  write_security_manifest "$staging/security" "$commit" "$(rustc_identity_sha256)"
  (
    cd "$staging/security"
    sha256sum harn-security-tests.tar.zst manifest > SHA256SUMS
    tar --zstd -cf "$security_output.tmp" harn-security-tests.tar.zst manifest SHA256SUMS
  )
  require_security_size_budget "$security_output.tmp"
  (
    cd "$staging"
    if [[ "$skip_neutral" != "1" ]]; then
      sha256sum harn-tests.tar.zst harn manifest > SHA256SUMS
      tar --zstd -cf "$output.tmp" harn-tests.tar.zst harn manifest SHA256SUMS
    fi
    sha256sum harn manifest > CLI_SHA256SUMS
    tar --zstd -cf "$cli_output.tmp" harn manifest CLI_SHA256SUMS
  )
  if [[ "$skip_neutral" != "1" ]]; then
    require_size_budget "$output.tmp"
    mv "$output.tmp" "$output"
  fi
  mv "$cli_output.tmp" "$cli_output"
  mv "$security_output.tmp" "$security_output"
  if [[ "$skip_neutral" != "1" ]]; then
    bytes="$(wc -c < "$output" | tr -d ' ')"
  else
    bytes="$(wc -c < "$security_output" | tr -d ' ')"
  fi
  report_timing build "$((SECONDS - started))" "$bytes"
}

restore_cli_bundle() {
  local bundle=$1
  local destination=$2
  local commit=$3
  local github_env=${4:-}
  local listing

  validate_commit "$commit"
  require_source_commit "$commit"
  if [[ -e "$destination" ]]; then
    echo "error: restore destination already exists: $destination" >&2
    exit 1
  fi

  listing="$(tar --zstd -tf "$bundle" | LC_ALL=C sort)"
  if [[ "$listing" != $'CLI_SHA256SUMS\nharn\nmanifest' ]]; then
    echo "error: CLI artifact has an unexpected file set" >&2
    printf '%s\n' "$listing" >&2
    exit 1
  fi

  mkdir -p "$destination"
  tar --zstd -xf "$bundle" -C "$destination"
  (
    cd "$destination"
    sha256sum -c CLI_SHA256SUMS
  )
  verify_cli_manifest "$destination" "$commit"
  if [[ ! -x "$destination/harn" ]]; then
    echo "error: restored harn CLI is not executable" >&2
    exit 1
  fi
  if [[ -n "$github_env" ]]; then
    printf 'HARN_BIN=%s\n' "$(cd "$destination" && pwd -P)/harn" >> "$github_env"
  fi
}

restore_bundle() {
  local bundle=$1
  local destination=$2
  local commit=$3
  local github_env=${4:-}
  local listing started bytes
  started=$SECONDS

  validate_commit "$commit"
  require_source_commit "$commit"
  require_size_budget "$bundle"
  if [[ -e "$destination" ]]; then
    echo "error: restore destination already exists: $destination" >&2
    exit 1
  fi

  listing="$(tar --zstd -tf "$bundle" | LC_ALL=C sort)"
  if [[ "$listing" != $'SHA256SUMS\nharn\nharn-tests.tar.zst\nmanifest' ]]; then
    echo "error: behavior artifact has an unexpected file set" >&2
    printf '%s\n' "$listing" >&2
    exit 1
  fi

  mkdir -p "$destination"
  tar --zstd -xf "$bundle" -C "$destination"
  (
    cd "$destination"
    sha256sum -c SHA256SUMS
  )
  verify_manifest "$destination" "$commit"
  if [[ ! -x "$destination/harn" ]]; then
    echo "error: restored harn CLI is not executable" >&2
    exit 1
  fi

  if [[ -n "$github_env" ]]; then
    printf 'HARN_BIN=%s\n' "$(cd "$destination" && pwd -P)/harn" >> "$github_env"
  fi
  bytes="$(wc -c < "$bundle" | tr -d ' ')"
  report_timing restore "$((SECONDS - started))" "$bytes"
}

restore_security_bundle() {
  local bundle=$1
  local destination=$2
  local commit=$3
  local listing started bytes
  started=$SECONDS

  validate_commit "$commit"
  require_source_commit "$commit"
  require_security_size_budget "$bundle"
  if [[ -e "$destination" ]]; then
    echo "error: restore destination already exists: $destination" >&2
    exit 1
  fi

  listing="$(tar --zstd -tf "$bundle" | LC_ALL=C sort)"
  if [[ "$listing" != $'SHA256SUMS\nharn-security-tests.tar.zst\nmanifest' ]]; then
    echo "error: security artifact has an unexpected file set" >&2
    printf '%s\n' "$listing" >&2
    exit 1
  fi

  mkdir -p "$destination"
  tar --zstd -xf "$bundle" -C "$destination"
  (
    cd "$destination"
    sha256sum -c SHA256SUMS
  )
  verify_security_manifest "$destination" "$commit"
  bytes="$(wc -c < "$bundle" | tr -d ' ')"
  report_timing restore "$((SECONDS - started))" "$bytes"
}

if [[ $# -lt 1 ]]; then
  usage >&2
  exit 2
fi

command=$1
shift
case "$command" in
  build)
    [[ $# -eq 4 ]] || { usage >&2; exit 2; }
    build_bundle "$@"
    ;;
  restore)
    [[ $# -ge 3 && $# -le 4 ]] || { usage >&2; exit 2; }
    restore_bundle "$@"
    ;;
  restore-cli)
    [[ $# -ge 3 && $# -le 4 ]] || { usage >&2; exit 2; }
    restore_cli_bundle "$@"
    ;;
  restore-security)
    [[ $# -eq 3 ]] || { usage >&2; exit 2; }
    restore_security_bundle "$@"
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
