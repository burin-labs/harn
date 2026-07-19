#!/usr/bin/env bash
set -euo pipefail

readonly NEXTEST_VERSION="0.9.132"
readonly NEUTRAL_FILTER='not test(test_linux_process_sandbox_catches_ten_process_escapes)'
readonly SECURITY_FILTER='package(harn-vm) and binary(harn_vm)'
readonly EXPECTED_RUSTFLAGS='-D warnings -Clink-arg=-fuse-ld=mold'
readonly EXPECTED_DEV_DEBUG='line-tables-only'
readonly DEFAULT_MAX_BUNDLE_BYTES=2147483648
cleanup_dir=""
trap '[[ -z "$cleanup_dir" ]] || rm -rf "$cleanup_dir"' EXIT

usage() {
  cat <<'EOF'
usage:
  scripts/ci/behavior_artifact.sh build <bundle.tar.zst> <commit-sha>
  scripts/ci/behavior_artifact.sh restore <bundle.tar.zst> <directory> <commit-sha> [github-env]
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
schema=harn.behavior_artifact.v2
commit=${commit}
nextest=${NEXTEST_VERSION}
rustc_sha256=${rustc_digest}
rust_toolchain_sha256=$(sha256 rust-toolchain.toml)
rustflags_sha256=$(printf '%s' "$EXPECTED_RUSTFLAGS" | sha256sum | cut -d ' ' -f 1)
dev_debug_sha256=$(printf '%s' "$EXPECTED_DEV_DEBUG" | sha256sum | cut -d ' ' -f 1)
neutral_filter_sha256=$(printf '%s' "$NEUTRAL_FILTER" | sha256sum | cut -d ' ' -f 1)
security_filter_sha256=$(printf '%s' "$SECURITY_FILTER" | sha256sum | cut -d ' ' -f 1)
harn_sha256=$(sha256 "$destination/harn")
neutral_archive_sha256=$(sha256 "$destination/harn-neutral.tar.zst")
security_archive_sha256=$(sha256 "$destination/harn-security.tar.zst")
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
  require_manifest_value "$destination/manifest" schema harn.behavior_artifact.v2
  require_manifest_value "$destination/manifest" commit "$commit"
  require_manifest_value "$destination/manifest" nextest "$NEXTEST_VERSION"
  require_manifest_value "$destination/manifest" rust_toolchain_sha256 "$(sha256 rust-toolchain.toml)"
  require_manifest_value "$destination/manifest" rustflags_sha256 "$(printf '%s' "$EXPECTED_RUSTFLAGS" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" dev_debug_sha256 "$(printf '%s' "$EXPECTED_DEV_DEBUG" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" neutral_filter_sha256 "$(printf '%s' "$NEUTRAL_FILTER" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" security_filter_sha256 "$(printf '%s' "$SECURITY_FILTER" | sha256sum | cut -d ' ' -f 1)"
  require_manifest_value "$destination/manifest" harn_sha256 "$(sha256 "$destination/harn")"
  require_manifest_value "$destination/manifest" neutral_archive_sha256 "$(sha256 "$destination/harn-neutral.tar.zst")"
  require_manifest_value "$destination/manifest" security_archive_sha256 "$(sha256 "$destination/harn-security.tar.zst")"

  if [[ "${HARN_VERIFY_RUST_RUNTIME:-0}" == "1" ]]; then
    require_nextest_version
    require_build_contract
    require_manifest_value "$destination/manifest" rustc_sha256 "$(rustc_identity_sha256)"
  fi
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
  local commit=$2
  local output_dir target_dir staging started bytes
  started=$SECONDS

  validate_commit "$commit"
  require_source_commit "$commit"
  require_nextest_version
  require_build_contract
  mkdir -p "$(dirname "$output")"
  output_dir="$(cd "$(dirname "$output")" && pwd -P)"
  output="${output_dir}/$(basename "$output")"
  target_dir="$(cargo metadata --format-version 1 --no-deps | jq -er '.target_directory')"
  staging="$(mktemp -d "${output_dir}/behavior-artifact.XXXXXX")"
  cleanup_dir="$staging"

  cargo build --locked --bin harn
  cargo nextest archive --locked --workspace --profile ci \
    -E "$NEUTRAL_FILTER" \
    --archive-file "$staging/harn-neutral.tar.zst"
  cargo nextest archive --locked --workspace --profile ci \
    -E "$SECURITY_FILTER" \
    --archive-file "$staging/harn-security.tar.zst"

  if [[ ! -x "$target_dir/debug/harn" ]]; then
    echo "error: build did not produce the required harn CLI at $target_dir/debug/harn" >&2
    exit 1
  fi
  install -m 0755 "$target_dir/debug/harn" "$staging/harn"
  write_manifest "$staging" "$commit" "$(rustc_identity_sha256)"
  (
    cd "$staging"
    sha256sum harn-neutral.tar.zst harn-security.tar.zst harn manifest > SHA256SUMS
    tar --zstd -cf "$output.tmp" harn-neutral.tar.zst harn-security.tar.zst harn manifest SHA256SUMS
  )
  require_size_budget "$output.tmp"
  mv "$output.tmp" "$output"
  bytes="$(wc -c < "$output" | tr -d ' ')"
  report_timing build "$((SECONDS - started))" "$bytes"
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

  listing="$(tar --zstd -tf "$bundle" | sort)"
  if [[ "$listing" != $'SHA256SUMS\nharn\nharn-neutral.tar.zst\nharn-security.tar.zst\nmanifest' ]]; then
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

if [[ $# -lt 1 ]]; then
  usage >&2
  exit 2
fi

command=$1
shift
case "$command" in
  build)
    [[ $# -eq 2 ]] || { usage >&2; exit 2; }
    build_bundle "$@"
    ;;
  restore)
    [[ $# -ge 3 && $# -le 4 ]] || { usage >&2; exit 2; }
    restore_bundle "$@"
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
