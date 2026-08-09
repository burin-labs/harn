#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <changed-files.txt>" >&2
  exit 64
fi

paths_file="$1"
if [ ! -r "$paths_file" ]; then
  echo "changed-files list not readable: $paths_file" >&2
  exit 66
fi

saw_path=false
while IFS= read -r path || [ -n "$path" ]; do
  path="${path#./}"
  if [ -z "$path" ]; then
    continue
  fi
  saw_path=true
  case "$path" in
    docs/* | website/* | *.md | */*.md)
      ;;
    *)
      echo false
      exit 0
      ;;
  esac
done < "$paths_file"

if [ "$saw_path" = true ]; then
  echo true
else
  echo false
fi
