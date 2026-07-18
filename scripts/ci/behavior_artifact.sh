#!/usr/bin/env bash
set -euo pipefail

readonly NEXTEST_VERSION="0.9.133"
readonly SECURITY_FILTER='package(harn-vm) and binary(harn_vm)'
cleanup_dir=""
trap '[[ -z "$cleanup_dir" ]] || rm -rf "$cleanup_dir"' EXIT

usage() {
  cat <<'EOF'
usage:
  scripts/ci/behavior_artifact.sh build <bundle.tar.zst> <commit-sha>
  scripts/ci/behavior_artifact.sh restore <bundle.tar.zst> <directory> <commit-sha> [github-env]
EOF
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

build_bundle() {
  local output=$1
  local commit=$2
  local output_dir target_dir staging

  validate_commit "$commit"
  require_nextest_version
  mkdir -p "$(dirname "$output")"
  output_dir="$(cd "$(dirname "$output")" && pwd -P)"
  output="${output_dir}/$(basename "$output")"
  target_dir="$(cargo metadata --format-version 1 --no-deps | jq -er '.target_directory')"
  staging="$(mktemp -d "${output_dir}/behavior-artifact.XXXXXX")"
  cleanup_dir="$staging"

  # Build the product executable explicitly. The filtered nextest archive is
  # allowed to omit unrelated binary targets, and a persistent target directory
  # must never let an older target/debug/harn leak into this commit's payload.
  cargo build --locked --bin harn
  cargo nextest archive --locked --workspace --profile ci \
    -E "$SECURITY_FILTER" \
    --archive-file "$staging/harn-security.tar.zst"

  if [[ ! -x "$target_dir/debug/harn" ]]; then
    echo "error: nextest did not build the required harn CLI at $target_dir/debug/harn" >&2
    exit 1
  fi
  install -m 0755 "$target_dir/debug/harn" "$staging/harn"
  printf 'schema=1\ncommit=%s\nnextest=%s\n' \
    "$commit" "$NEXTEST_VERSION" > "$staging/manifest"
  (
    cd "$staging"
    sha256sum harn-security.tar.zst harn manifest > SHA256SUMS
    tar --zstd -cf "$output.tmp" harn-security.tar.zst harn manifest SHA256SUMS
  )
  mv "$output.tmp" "$output"
  printf '%s %s bytes\n' "$output" "$(wc -c < "$output" | tr -d ' ')"
}

restore_bundle() {
  local bundle=$1
  local destination=$2
  local commit=$3
  local github_env=${4:-}
  local listing expected_manifest

  validate_commit "$commit"
  if [[ -e "$destination" ]]; then
    echo "error: restore destination already exists: $destination" >&2
    exit 1
  fi

  listing="$(tar --zstd -tf "$bundle" | sort)"
  if [[ "$listing" != $'SHA256SUMS\nharn\nharn-security.tar.zst\nmanifest' ]]; then
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
  expected_manifest="$(printf 'schema=1\ncommit=%s\nnextest=%s' "$commit" "$NEXTEST_VERSION")"
  if [[ "$(cat "$destination/manifest")" != "$expected_manifest" ]]; then
    echo "error: behavior artifact manifest does not match this CI execution" >&2
    exit 1
  fi
  if [[ ! -x "$destination/harn" ]]; then
    echo "error: restored harn CLI is not executable" >&2
    exit 1
  fi

  if [[ -n "$github_env" ]]; then
    printf 'HARN_BIN=%s\n' "$(cd "$destination" && pwd -P)/harn" >> "$github_env"
  fi
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
