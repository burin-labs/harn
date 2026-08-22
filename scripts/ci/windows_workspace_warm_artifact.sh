#!/usr/bin/env bash
# Main-produced Windows workspace warm target outside the Actions cache
# namespace. Release-certify consumers restore it read-only and fall back cold
# when no compatible generation exists (harn#6485).
#
# Knobs live in .github/cache-policy.json; this script and
# scripts/check_ci_cache_policy.harn both read that document through
# scripts/ci/cache_policy.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/ci/cache_policy.sh
source "${SCRIPT_DIR}/cache_policy.sh"

load_warm_policy() {
  ARTIFACT_NAME="$(harn_cache_policy_jq '.windows_workspace_warm.artifact_name')"
  SCHEMA="$(harn_cache_policy_jq '.windows_workspace_warm.manifest_schema')"
  WORKFLOW_FILE="$(harn_cache_policy_jq '.windows_workspace_warm.workflow')"
  DEFAULT_MAX_BYTES="$(harn_cache_policy_jq '.windows_workspace_warm.max_bytes')"
  DEFAULT_BUILD_HEADROOM_BYTES="$(harn_cache_policy_jq '.windows_workspace_warm.build_headroom_bytes')"
  NEXTEST_VERSION="$(harn_cache_policy_jq '.nextest_version')"
  PRODUCER_REF="$(harn_cache_policy_jq '.persistent_ref')"
  PRODUCER_BRANCH="${PRODUCER_REF#refs/heads/}"
  if [[ "$PRODUCER_BRANCH" == "$PRODUCER_REF" || -z "$PRODUCER_BRANCH" ]]; then
    echo "error: persistent_ref must be refs/heads/<branch>, got ${PRODUCER_REF}" >&2
    exit 2
  fi
}

usage() {
  cat <<'EOF'
usage:
  scripts/ci/windows_workspace_warm_artifact.sh pack <staging-dir>
  scripts/ci/windows_workspace_warm_artifact.sh restore <staging-dir> <target-dir>
  scripts/ci/windows_workspace_warm_artifact.sh discover --repo OWNER/REPO
  scripts/ci/windows_workspace_warm_artifact.sh download-and-restore --repo OWNER/REPO [--target-dir DIR]

pack strips workspace-member and incremental outputs from CARGO_TARGET_DIR, then
writes a typed warm bundle under <staging-dir> for actions/upload-artifact.

download-and-restore finds the newest successful main windows-nightly warm
artifact, restores it into the target dir, and exits 0 only on a compatible hit.
Missing or incompatible generations exit non-zero so the caller can fall cold.

Configuration is owned by .github/cache-policy.json (nextest_version and
windows_workspace_warm).
EOF
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

sha256_string() {
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$1" | sha256sum | awk '{ print $1 }'
  else
    printf '%s' "$1" | shasum -a 256 | awk '{ print $1 }'
  fi
}

rustc_identity_sha256() {
  rustc -vV | if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{ print $1 }'
  else
    shasum -a 256 | awk '{ print $1 }'
  fi
}

resolve_target_dir() {
  local override=${1:-}
  if [[ -n "$override" ]]; then
    printf '%s\n' "$override"
    return 0
  fi
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
    return 0
  fi
  cargo metadata --format-version 1 --no-deps | jq -er '.target_directory'
}

require_repo() {
  if [[ ! "$1" =~ ^[^/]+/[^/]+$ ]]; then
    echo "error: expected repo OWNER/REPO, got '$1'" >&2
    exit 2
  fi
}

max_bundle_bytes() {
  local value="${HARN_WINDOWS_WARM_ARTIFACT_MAX_BYTES:-$DEFAULT_MAX_BYTES}"
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: HARN_WINDOWS_WARM_ARTIFACT_MAX_BYTES must be a positive integer" >&2
    exit 2
  fi
  printf '%s\n' "$value"
}

build_headroom_bytes() {
  local value="${HARN_WINDOWS_WARM_BUILD_HEADROOM_BYTES:-$DEFAULT_BUILD_HEADROOM_BYTES}"
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: HARN_WINDOWS_WARM_BUILD_HEADROOM_BYTES must be a positive integer" >&2
    exit 2
  fi
  printf '%s\n' "$value"
}

directory_bytes() {
  local path=$1
  local kib
  kib="$(du -sk "$path" | awk '{ print $1 }')"
  if [[ ! "$kib" =~ ^[0-9]+$ ]]; then
    echo "error: could not measure directory bytes for $path" >&2
    exit 1
  fi
  printf '%s\n' "$((kib * 1024))"
}

filesystem_free_bytes() {
  local path=$1
  local kib
  kib="$(df -Pk "$path" | awk 'NR > 1 { value = $4 } END { print value }')"
  if [[ ! "$kib" =~ ^[0-9]+$ ]]; then
    echo "error: could not measure filesystem free bytes for $path" >&2
    exit 1
  fi
  printf '%s\n' "$((kib * 1024))"
}

write_manifest() {
  local destination=$1
  local producer_commit=$2
  local target_bytes=$3
  cat > "$destination/manifest" <<EOF
schema=${SCHEMA}
artifact=${ARTIFACT_NAME}
producer_commit=${producer_commit}
producer_ref=${PRODUCER_REF}
nextest=${NEXTEST_VERSION}
rustc_sha256=$(rustc_identity_sha256)
rust_toolchain_sha256=$(sha256 rust-toolchain.toml)
rustflags_sha256=$(sha256_string "${RUSTFLAGS:-}")
cargo_incremental=${CARGO_INCREMENTAL:-}
target_bytes=${target_bytes}
EOF
}

require_manifest_value() {
  local manifest=$1
  local key=$2
  local expected=$3
  local actual
  actual="$(sed -n "s/^${key}=//p" "$manifest")"
  if [[ -z "$actual" || "$actual" != "$expected" ]]; then
    echo "error: warm artifact manifest ${key} mismatch (expected ${expected}, got ${actual:-<missing>})" >&2
    exit 1
  fi
}

verify_compat_manifest() {
  local manifest=$1
  require_manifest_value "$manifest" schema "$SCHEMA"
  require_manifest_value "$manifest" artifact "$ARTIFACT_NAME"
  require_manifest_value "$manifest" rust_toolchain_sha256 "$(sha256 rust-toolchain.toml)"
  require_manifest_value "$manifest" rustflags_sha256 "$(sha256_string "${RUSTFLAGS:-}")"
  require_manifest_value "$manifest" cargo_incremental "${CARGO_INCREMENTAL:-}"
  local target_bytes
  target_bytes="$(sed -n 's/^target_bytes=//p' "$manifest")"
  if [[ ! "$target_bytes" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: warm artifact manifest target_bytes must be a positive integer" >&2
    exit 1
  fi
}

strip_workspace_member_artifacts() {
  local target_dir=$1
  local package_json names name underscored
  package_json="$(cargo metadata --format-version 1 --no-deps)"
  names="$(jq -r '.packages[].name' <<<"$package_json")"
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    underscored="${name//-/_}"
    find "$target_dir" -mindepth 2 -maxdepth 3 \( \
      -name "${name}-*" -o \
      -name "${underscored}-*" -o \
      -name "lib${underscored}-*" -o \
      -name "${name}.exe" -o \
      -name "${name}.pdb" -o \
      -name "${underscored}.exe" -o \
      -name "${underscored}.pdb" \
      \) -exec rm -rf {} + 2>/dev/null || true
  done <<<"$names"
  find "$target_dir" -type d -name incremental -prune -exec rm -rf {} + 2>/dev/null || true
}

pack() {
  local staging=$1
  local target_dir producer_commit archive bytes limit target_bytes
  if [[ -z "$staging" ]]; then
    usage >&2
    exit 2
  fi
  target_dir="$(resolve_target_dir)"
  if [[ ! -d "$target_dir" ]]; then
    echo "error: CARGO_TARGET_DIR does not exist: $target_dir" >&2
    exit 1
  fi
  producer_commit="$(git rev-parse --verify HEAD)"
  if [[ ! "$producer_commit" =~ ^[0-9a-f]{40}$ ]]; then
    echo "error: expected a full commit SHA for the warm producer" >&2
    exit 1
  fi

  strip_workspace_member_artifacts "$target_dir"
  target_bytes="$(directory_bytes "$target_dir")"
  rm -rf "$staging"
  mkdir -p "$staging"
  write_manifest "$staging" "$producer_commit" "$target_bytes"
  archive="$staging/target.tar.gz"
  # Write the archive via a basename so Git Bash tar does not treat Windows
  # drive-letter staging paths (D:\a\_temp\...) as remote hosts.
  (
    cd "$staging"
    tar -czf target.tar.gz -C "$target_dir" .
  )
  bytes="$(wc -c < "$archive" | tr -d ' ')"
  limit="$(max_bundle_bytes)"
  if (( bytes > limit )); then
    echo "error: Windows warm artifact is ${bytes} bytes; limit is ${limit} bytes" >&2
    exit 1
  fi
  printf 'windows_warm_artifact_bytes=%s\n' "$bytes"
  printf 'windows_warm_artifact_target_bytes=%s\n' "$target_bytes"
  printf 'windows_warm_artifact_producer_commit=%s\n' "$producer_commit"
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "### Windows workspace warm artifact"
      echo
      echo "- Producer commit: \`${producer_commit}\`"
      echo "- Compressed bytes: ${bytes}"
      echo "- Restored target bytes: ${target_bytes}"
      echo "- Artifact name: \`${ARTIFACT_NAME}\`"
    } >> "$GITHUB_STEP_SUMMARY"
  fi
}

restore() {
  local staging=$1
  local target_dir=$2
  local archive manifest target_bytes headroom_bytes free_bytes current_target_bytes
  local available_after_replace_bytes required_bytes
  if [[ -z "$staging" || -z "$target_dir" ]]; then
    usage >&2
    exit 2
  fi
  manifest="$staging/manifest"
  archive="$staging/target.tar.gz"
  if [[ ! -f "$manifest" || ! -f "$archive" ]]; then
    echo "error: warm artifact staging is incomplete under $staging" >&2
    exit 1
  fi
  verify_compat_manifest "$manifest"
  mkdir -p "$target_dir"
  # The caller may use Cargo's workspace-relative default (`target`). Resolve
  # it before entering the artifact staging directory so tar restores into the
  # workspace target rather than looking for a sibling under staging.
  target_dir="$(cd "$target_dir" && pwd -P)"
  target_bytes="$(sed -n 's/^target_bytes=//p' "$manifest")"
  headroom_bytes="$(build_headroom_bytes)"
  free_bytes="$(filesystem_free_bytes "$target_dir")"
  current_target_bytes="$(directory_bytes "$target_dir")"
  available_after_replace_bytes="$((free_bytes + current_target_bytes))"
  required_bytes="$((target_bytes + headroom_bytes))"
  if (( available_after_replace_bytes < required_bytes )); then
    printf 'windows_warm_restore_reason=insufficient_space\n'
    printf 'windows_warm_restore_target_bytes=%s\n' "$target_bytes"
    printf 'windows_warm_restore_headroom_bytes=%s\n' "$headroom_bytes"
    printf 'windows_warm_restore_free_bytes=%s\n' "$free_bytes"
    printf 'windows_warm_restore_existing_target_bytes=%s\n' "$current_target_bytes"
    printf 'windows_warm_restore_available_after_replace_bytes=%s\n' "$available_after_replace_bytes"
    echo "error: warm artifact needs ${target_bytes} target bytes plus ${headroom_bytes} build-headroom bytes, but replacing the existing target would make only ${available_after_replace_bytes} bytes available" >&2
    exit 1
  fi
  # Replace any partial target so restored deps own the tree.
  find "$target_dir" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  (
    cd "$(dirname "$archive")"
    tar -xzf "$(basename "$archive")" -C "$target_dir"
  )
  printf 'windows_warm_restore=hit\n'
  printf 'windows_warm_restore_target_bytes=%s\n' "$target_bytes"
  printf 'windows_warm_restore_headroom_bytes=%s\n' "$headroom_bytes"
  printf 'windows_warm_restore_free_bytes_before=%s\n' "$free_bytes"
  printf 'windows_warm_restore_existing_target_bytes=%s\n' "$current_target_bytes"
  printf 'windows_warm_restore_available_after_replace_bytes=%s\n' "$available_after_replace_bytes"
  printf 'windows_warm_restore_producer_commit=%s\n' \
    "$(sed -n 's/^producer_commit=//p' "$manifest")"
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "### Windows workspace warm restore"
      echo
      echo "- Outcome: hit"
      echo "- Producer commit: \`$(sed -n 's/^producer_commit=//p' "$manifest")\`"
    } >> "$GITHUB_STEP_SUMMARY"
  fi
}

discover() {
  local repo=""
  while (($#)); do
    case "$1" in
      --repo) repo="${2:-}"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
  done
  if [[ -z "$repo" ]]; then
    echo "error: --repo is required" >&2
    usage >&2
    exit 2
  fi
  require_repo "$repo"

  local runs_json run_id artifacts_json
  runs_json="$(gh api \
    "repos/${repo}/actions/workflows/${WORKFLOW_FILE}/runs?branch=${PRODUCER_BRANCH}&status=completed&per_page=30")"
  while IFS= read -r run_json; do
    [[ -n "$run_json" ]] || continue
    if [[ "$(jq -r '.conclusion // empty' <<<"$run_json")" != "success" ]]; then
      continue
    fi
    run_id="$(jq -r '.id // empty' <<<"$run_json")"
    [[ -n "$run_id" ]] || continue
    artifacts_json="$(gh api "repos/${repo}/actions/runs/${run_id}/artifacts")"
    if jq -e --arg name "$ARTIFACT_NAME" \
      'any(.artifacts[]?; (.name == $name) and (.expired | not) and (.size_in_bytes > 0))' \
      <<<"$artifacts_json" >/dev/null; then
      printf '%s\n' "$run_id"
      return 0
    fi
  done < <(jq -c '.workflow_runs[]?' <<<"$runs_json")

  echo "error: no successful ${PRODUCER_BRANCH} ${WORKFLOW_FILE} run has a live ${ARTIFACT_NAME} artifact" >&2
  exit 1
}

download_and_restore() {
  local repo=""
  local target_dir=""
  while (($#)); do
    case "$1" in
      --repo) repo="${2:-}"; shift 2 ;;
      --target-dir) target_dir="${2:-}"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
  done
  if [[ -z "$repo" ]]; then
    echo "error: --repo is required" >&2
    usage >&2
    exit 2
  fi
  require_repo "$repo"
  target_dir="$(resolve_target_dir "$target_dir")"

  local run_id staging
  run_id="$(discover --repo "$repo")"
  staging="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/${ARTIFACT_NAME}.XXXXXX")"
  cleanup_dir="$staging"
  trap '[[ -z "${cleanup_dir:-}" ]] || rm -rf "$cleanup_dir"' EXIT

  gh run download "$run_id" --repo "$repo" --name "$ARTIFACT_NAME" --dir "$staging"
  # upload-artifact may nest files directly or under the artifact name.
  if [[ ! -f "$staging/manifest" && -f "$staging/$ARTIFACT_NAME/manifest" ]]; then
    staging="$staging/$ARTIFACT_NAME"
  fi
  printf 'windows_warm_restore_run_id=%s\n' "$run_id"
  restore "$staging" "$target_dir"
}

main() {
  local command=${1:-}
  if [[ -z "$command" ]]; then
    usage >&2
    exit 2
  fi
  shift
  case "$command" in
    -h|--help) usage; return 0 ;;
  esac
  load_warm_policy
  case "$command" in
    pack) pack "$@" ;;
    restore) restore "$@" ;;
    discover) discover "$@" ;;
    download-and-restore) download_and_restore "$@" ;;
    *) echo "error: unknown command: $command" >&2; usage >&2; exit 2 ;;
  esac
}

main "$@"
