#!/usr/bin/env bash
set -euo pipefail

# bubblewrap is installed from a cached .deb so a stalled package mirror cannot
# fail the job before any test runs. The cache is filled from apt's own
# download the first time; this script only ever installs what apt produced.
readonly DPKG_SECONDS="${DPKG_SECONDS:-60}"

output() {
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf '%s\n' "$1" >>"$GITHUB_OUTPUT"
  fi
}

# shellcheck source=/dev/null
release="$(. /etc/os-release && printf '%s' "${VERSION_ID:-unknown}")"
arch="$(dpkg --print-architecture)"
candidate="$(apt-cache policy bubblewrap 2>/dev/null | awk '/Candidate:/ {print $2}')"
dir="${RUNNER_TEMP:-/tmp}/bubblewrap-debs"

case "${1:-install}" in
  dir)
    mkdir -p "$dir"
    output "dir=${dir}"
    output "key=bubblewrap-${release}-${arch}-${candidate:-unknown}-v1"
    ;;
  install)
    if command -v bwrap >/dev/null 2>&1; then
      bwrap --version
      output "installed=true"
      exit 0
    fi
    shopt -s nullglob
    debs=("$dir"/bubblewrap_*.deb)
    if [[ "${#debs[@]}" -gt 0 ]] \
      && sudo timeout --signal=TERM --kill-after=5s "${DPKG_SECONDS}s" dpkg -i "${debs[@]}" \
      && command -v bwrap >/dev/null 2>&1; then
      bwrap --version
      output "installed=true"
    else
      echo "::warning::no usable cached bubblewrap package; falling back to apt"
      output "installed=false"
    fi
    ;;
  stash)
    # After an apt install, keep the package apt fetched for the next job.
    shopt -s nullglob
    for deb in /var/cache/apt/archives/bubblewrap_*.deb; do
      cp "$deb" "$dir/"
    done
    ;;
esac
