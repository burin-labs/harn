#!/usr/bin/env bash
set -euo pipefail

if (($# > 1)); then
  echo 'usage: prepare_dist_build.sh [target]' >&2
  exit 2
fi
: "${GITHUB_ENV:?GITHUB_ENV must name the job environment file}"
case "${RUNNER_OS:-}" in
  Linux | macOS | Windows) ;;
  *) echo 'dist build: missing or unsupported RUNNER_OS' >&2; exit 1 ;;
esac
target=${1:-$(rustc -vV | awk '$1 == "host:" {print $2}')}
if [[ -z "$target" || "$target" == *$'\n'* ]]; then
  echo 'dist build: target was not measured' >&2
  exit 1
fi

# These targets cannot reliably link the fat-LTO profile on their release hosts.
case "$target" in
  aarch64-unknown-linux-gnu | aarch64-apple-darwin | x86_64-apple-darwin)
    {
      echo 'CARGO_PROFILE_DIST_LTO=thin'
      echo 'CARGO_PROFILE_DIST_CODEGEN_UNITS=16'
    } >> "$GITHUB_ENV"
    echo "dist profile for $target: thin LTO, 16 codegen units"
    ;;
  *) echo "dist profile for $target: Cargo.toml defaults" ;;
esac

if [[ "$RUNNER_OS" == Linux ]]; then
  swap_mib=$(free -m | awk '$1 == "Swap:" {print $2}')
  if [[ ! "$swap_mib" =~ ^[0-9]+$ ]]; then
    echo 'dist build: swap capacity was not measured' >&2
    exit 1
  fi
  if ((swap_mib < 12288)); then
    sudo swapoff -a || true
    sudo fallocate -l 16G /mnt/harn-release.swap
    sudo chmod 600 /mnt/harn-release.swap
    sudo mkswap /mnt/harn-release.swap >/dev/null
    sudo swapon /mnt/harn-release.swap
  fi
  free -m
  df -h /mnt "$GITHUB_WORKSPACE"
fi
