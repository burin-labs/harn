#!/usr/bin/env bash
# Main-produced Windows workspace warm target outside the 10 GiB Actions cache
# namespace. Release-certify consumers restore it read-only and fall back cold
# when no compatible generation exists (harn#6485).
set -euo pipefail

readonly ARTIFACT_NAME="workspace-windows-warm"
readonly SCHEMA="harn.windows_workspace_warm.v1"
readonly WORKFLOW_FILE="windows-nightly.yml"
readonly DEFAULT_MAX_BYTES=8589934592  # 8 GiB compressed
readonly NEXTEST_VERSION="0.9.132"

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

write_manifest() {
  local destination=$1
  local producer_commit=$2
  cat > "$destination/manifest" <<EOF
schema=${SCHEMA}
artifact=${ARTIFACT_NAME}
producer_commit=${producer_commit}
producer_ref=refs/heads/main
nextest=${NEXTEST_VERSION}
rustc_sha256=$(rustc_identity_sha256)
rust_toolchain_sha256=$(sha256 rust-toolchain.toml)
rustflags_sha256=$(sha256_string "${RUSTFLAGS:-}")
cargo_incremental=${CARGO_INCREMENTAL:-}
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
  local target_dir producer_commit archive bytes limit
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
  rm -rf "$staging"
  mkdir -p "$staging"
  write_manifest "$staging" "$producer_commit"
  archive="$staging/target.tar.gz"
  tar -czf "$archive" -C "$target_dir" .
  bytes="$(wc -c < "$archive" | tr -d ' ')"
  limit="$(max_bundle_bytes)"
  if (( bytes > limit )); then
    echo "error: Windows warm artifact is ${bytes} bytes; limit is ${limit} bytes" >&2
    exit 1
  fi
  printf 'windows_warm_artifact_bytes=%s\n' "$bytes"
  printf 'windows_warm_artifact_producer_commit=%s\n' "$producer_commit"
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "### Windows workspace warm artifact"
      echo
      echo "- Producer commit: \`${producer_commit}\`"
      echo "- Compressed bytes: ${bytes}"
      echo "- Artifact name: \`${ARTIFACT_NAME}\`"
    } >> "$GITHUB_STEP_SUMMARY"
  fi
}

restore() {
  local staging=$1
  local target_dir=$2
  local archive manifest
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
  # Replace any partial target so restored deps own the tree.
  find "$target_dir" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  tar -xzf "$archive" -C "$target_dir"
  printf 'windows_warm_restore=hit\n'
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
    "repos/${repo}/actions/workflows/${WORKFLOW_FILE}/runs?branch=main&status=completed&per_page=30")"
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

  echo "error: no successful main ${WORKFLOW_FILE} run has a live ${ARTIFACT_NAME} artifact" >&2
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
  staging="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/workspace-windows-warm.XXXXXX")"
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
    pack) pack "$@" ;;
    restore) restore "$@" ;;
    discover) discover "$@" ;;
    download-and-restore) download_and_restore "$@" ;;
    -h|--help) usage ;;
    *) echo "error: unknown command: $command" >&2; usage >&2; exit 2 ;;
  esac
}

main "$@"
