#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
source_path="$repo_root/scripts/config/host-bound-rust-tests.txt"
if (( $# > 2 )); then
  echo "usage: host_bound_rust_test_filter.sh [linux|macos|windows] [filter|names]" >&2
  exit 2
fi
platform="${1:-}"
format="${2:-filter}"

if [[ -z "$platform" ]]; then
  case "$(uname -s)" in
    Linux*) platform=linux ;;
    Darwin*) platform=macos ;;
    MINGW*|MSYS*|CYGWIN*) platform=windows ;;
    *) echo "unsupported host-bound Rust test platform: $(uname -s)" >&2; exit 1 ;;
  esac
fi
case "$platform" in linux|macos|windows) ;; *) echo "invalid host-bound Rust test platform: $platform" >&2; exit 1 ;; esac
case "$format" in filter|names) ;; *) echo "invalid host-bound Rust test filter format: $format" >&2; exit 1 ;; esac

filter=""
count=0

while IFS=' ' read -r scope test_name extra || [[ -n "${scope:-}${test_name:-}${extra:-}" ]]; do
  [[ -n "${scope:-}" ]] || continue
  if [[ -z "${test_name:-}" || -n "${extra:-}" ]]; then
    echo "invalid host-bound Rust test registry row: $scope $test_name ${extra:-}" >&2
    exit 1
  fi
  case "$scope" in all|linux|macos|unix|windows) ;; *) echo "invalid host-bound Rust test scope: $scope" >&2; exit 1 ;; esac
  if [[ ! "$test_name" =~ ^[A-Za-z0-9_]+$ ]]; then
    echo "invalid host-bound Rust test name: $test_name" >&2
    exit 1
  fi
  case "$scope:$platform" in
    all:*|linux:linux|macos:macos|unix:linux|unix:macos|windows:windows) ;;
    *) continue ;;
  esac
  if [[ "$format" == names ]]; then
    printf '%s\n' "$test_name"
    ((count += 1))
    continue
  fi
  if ((count > 0)); then
    filter+=" or "
  fi
  filter+="test(/(^|::)${test_name}(::|$)/)"
  ((count += 1))
done < "$source_path"

if ((count == 0)); then
  echo "host-bound Rust test list is empty for platform $platform" >&2
  exit 1
fi

if [[ "$format" == filter ]]; then printf '%s\n' "$filter"; fi
