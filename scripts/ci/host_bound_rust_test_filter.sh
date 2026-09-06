#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
source_path="$repo_root/scripts/config/host-bound-rust-tests.txt"
filter=""
count=0

while IFS= read -r test_name || [[ -n "$test_name" ]]; do
  [[ -n "$test_name" ]] || continue
  if [[ ! "$test_name" =~ ^[A-Za-z0-9_]+$ ]]; then
    echo "invalid host-bound Rust test name: $test_name" >&2
    exit 1
  fi
  if ((count > 0)); then
    filter+=" or "
  fi
  filter+="test(${test_name})"
  ((count += 1))
done < "$source_path"

if ((count == 0)); then
  echo "host-bound Rust test list is empty" >&2
  exit 1
fi

printf '%s\n' "$filter"
