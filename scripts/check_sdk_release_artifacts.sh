#!/usr/bin/env bash
# Fail closed unless both generated SDK language artifacts are present.
#
# Two inspection modes:
#   --dir DIR       require python/ and typescript/ trees under DIR, each with
#                   a harn-sdk-generation.txt that records harn_version and
#                   openapi_sha256
#   --release TAG   require durable GitHub release assets named
#                   harn-sdk-python.tar.gz and harn-sdk-typescript.tar.gz
set -euo pipefail

# Quote language names so the python-boundary scan does not treat this as a
# `python` invocation (it matches `(` + `python`).
REQUIRED_LANGUAGES=("python" "typescript")
MANIFEST_NAME="harn-sdk-generation.txt"

usage() {
  cat <<'USAGE'
usage: scripts/check_sdk_release_artifacts.sh --dir DIR
       scripts/check_sdk_release_artifacts.sh --release TAG [--repo OWNER/REPO]

Fail closed if either generated SDK language artifact is missing.
USAGE
}

mode=""
artifact_dir=""
release_tag=""
repo="${GITHUB_REPOSITORY:-burin-labs/harn}"
gh_bin="${HARN_GH_BIN:-gh}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir)
      mode="dir"
      artifact_dir="${2:-}"
      shift 2
      ;;
    --release)
      mode="release"
      release_tag="${2:-}"
      shift 2
      ;;
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    --gh-bin)
      gh_bin="${2:-}"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$mode" ]]; then
  usage >&2
  exit 2
fi

release_asset_name() {
  printf 'harn-sdk-%s.tar.gz\n' "$1"
}

manifest_field() {
  local manifest="$1"
  local key="$2"
  awk -F= -v key="$key" '$1 == key { print $2; exit }' "$manifest"
}

check_manifest() {
  local language="$1"
  local manifest="$2"
  local missing=()

  if [[ ! -s "$manifest" ]]; then
    echo "missing ${language} SDK generation manifest: $manifest" >&2
    return 1
  fi

  local recorded_language harn_version openapi_sha256
  recorded_language="$(manifest_field "$manifest" language)"
  harn_version="$(manifest_field "$manifest" harn_version)"
  openapi_sha256="$(manifest_field "$manifest" openapi_sha256)"

  if [[ "$recorded_language" != "$language" ]]; then
    missing+=("language=${language}")
  fi
  if [[ -z "$harn_version" ]]; then
    missing+=("harn_version")
  fi
  if [[ -z "$openapi_sha256" ]]; then
    missing+=("openapi_sha256")
  fi

  if (( ${#missing[@]} > 0 )); then
    printf 'incomplete %s SDK generation manifest %s: missing %s\n' \
      "$language" "$manifest" "${missing[*]}" >&2
    return 1
  fi

  printf 'ok %s language=%s harn_version=%s openapi_sha256=%s\n' \
    "$language" "$recorded_language" "$harn_version" "$openapi_sha256"
}

find_language_dir() {
  local root="$1"
  local language="$2"
  local candidate

  if [[ -s "${root}/${language}/${MANIFEST_NAME}" ]]; then
    printf '%s/%s\n' "$root" "$language"
    return 0
  fi

  shopt -s nullglob
  for candidate in "${root}/harn-sdk-${language}-"*; do
    if [[ -s "${candidate}/${MANIFEST_NAME}" ]]; then
      printf '%s\n' "$candidate"
      shopt -u nullglob
      return 0
    fi
  done
  shopt -u nullglob
  return 1
}

check_dir() {
  if [[ -z "$artifact_dir" ]]; then
    echo "--dir requires a directory" >&2
    usage >&2
    exit 2
  fi
  if [[ ! -d "$artifact_dir" ]]; then
    echo "SDK artifact directory does not exist: $artifact_dir" >&2
    exit 1
  fi

  local language language_dir missing=()
  for language in "${REQUIRED_LANGUAGES[@]}"; do
    if ! language_dir="$(find_language_dir "$artifact_dir" "$language")"; then
      missing+=("$language")
      continue
    fi
    if ! check_manifest "$language" "${language_dir}/${MANIFEST_NAME}"; then
      missing+=("$language")
    fi
  done

  if (( ${#missing[@]} > 0 )); then
    printf 'SDK release artifacts missing or incomplete: %s\n' "${missing[*]}" >&2
    exit 1
  fi

  echo "both SDK language artifacts are present under $artifact_dir"
}

check_release() {
  if [[ -z "$release_tag" ]]; then
    echo "--release requires a tag" >&2
    usage >&2
    exit 2
  fi
  if [[ -z "$repo" ]]; then
    echo "--repo is required when GITHUB_REPOSITORY is unset" >&2
    exit 2
  fi

  local assets missing=() language expected
  if ! assets="$("$gh_bin" release view "$release_tag" --repo "$repo" --json assets --jq '.assets[].name')"; then
    echo "failed to list GitHub release assets for $release_tag in $repo" >&2
    exit 1
  fi

  for language in "${REQUIRED_LANGUAGES[@]}"; do
    expected="$(release_asset_name "$language")"
    if ! grep -Fxq "$expected" <<<"$assets"; then
      missing+=("$expected")
    fi
  done

  if (( ${#missing[@]} > 0 )); then
    printf 'GitHub release %s is missing SDK artifacts: %s\n' \
      "$release_tag" "${missing[*]}" >&2
    exit 1
  fi

  echo "both SDK language artifacts are attached to $release_tag"
}

case "$mode" in
  dir) check_dir ;;
  release) check_release ;;
  *)
    usage >&2
    exit 2
    ;;
esac
