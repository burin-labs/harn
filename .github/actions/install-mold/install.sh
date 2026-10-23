#!/usr/bin/env bash
set -euo pipefail

# mold comes from its pinned upstream release, not the distribution mirror: a
# stalled apt mirror used to fail the step before any test ran. The checksums
# below are the release assets' published sha256 digests.
readonly MOLD_VERSION="2.40.4"
readonly MOLD_DOWNLOAD_SECONDS="${MOLD_DOWNLOAD_SECONDS:-60}"

case "$(uname -m)" in
  x86_64)
    arch="x86_64"
    sha256="4c999e19ffa31afa5aa429c679b665d5e2ca5a6b6832ad4b79668e8dcf3d8ec1"
    ;;
  aarch64 | arm64)
    arch="aarch64"
    sha256="c799b9ccae8728793da2186718fbe53b76400a9da396184fac0c64aa3298ec37"
    ;;
  *)
    arch=""
    sha256=""
    ;;
esac

root="${RUNNER_TOOL_CACHE:-${RUNNER_TEMP:-/tmp}}"
dir="${root}/mold-${MOLD_VERSION}-${arch:-unsupported}"

output() {
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf '%s\n' "$1" >>"$GITHUB_OUTPUT"
  fi
}

if [[ "${1:-install}" == "dir" ]]; then
  output "dir=${dir}"
  output "key=mold-${MOLD_VERSION}-${arch:-unsupported}"
  exit 0
fi

use_installed() {
  if [[ -n "${GITHUB_PATH:-}" ]]; then
    printf '%s\n' "$1" >>"$GITHUB_PATH"
  fi
  "$1/mold" --version
  output "installed=true"
}

if command -v mold >/dev/null 2>&1; then
  mold --version
  output "installed=true"
  exit 0
fi

if [[ -x "${dir}/bin/mold" ]]; then
  use_installed "${dir}/bin"
  exit 0
fi

if [[ -z "$arch" ]]; then
  echo "::warning::no pinned mold release for $(uname -m); falling back to the package manager"
  output "installed=false"
  exit 0
fi

archive="mold-${MOLD_VERSION}-${arch}-linux.tar.gz"
url="https://github.com/rui314/mold/releases/download/v${MOLD_VERSION}/${archive}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# One bounded attempt: the apt fallback is the retry, and it needs most of the
# step's five minutes if the download stalls.
if ! curl --fail --silent --show-error --location \
  --connect-timeout 10 --max-time "$MOLD_DOWNLOAD_SECONDS" \
  --output "${work}/${archive}" "$url"; then
  echo "::warning::could not download ${archive}; falling back to the package manager"
  output "installed=false"
  exit 0
fi

if ! printf '%s  %s\n' "$sha256" "${work}/${archive}" | sha256sum --check --status; then
  echo "::error::${archive} does not match its pinned sha256; refusing to install it"
  exit 1
fi

mkdir -p "$dir"
tar -xzf "${work}/${archive}" -C "$dir" --strip-components=1
use_installed "${dir}/bin"
