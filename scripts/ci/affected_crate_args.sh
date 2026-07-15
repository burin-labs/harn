#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
global_paths_file="$repo_root/scripts/config/affected-crate-global-paths.txt"
base="origin/main"
changed_files_file=""

usage() {
  cat <<'EOF'
usage: scripts/ci/affected_crate_args.sh [--base REF] [--changed-files-file PATH]

Print cargo-nextest package args for the PR affected-crate test lane. Global
workspace paths are handled in this cheap shell wrapper because they always
select `--workspace`; partial crate selection remains delegated to
scripts/affected-crates.harn.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      if [[ $# -lt 2 ]]; then
        echo "error: --base requires a ref" >&2
        exit 2
      fi
      base="$2"
      shift 2
      ;;
    --changed-files-file)
      if [[ $# -lt 2 ]]; then
        echo "error: --changed-files-file requires a path" >&2
        exit 2
      fi
      changed_files_file="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unexpected argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

mapfile -t global_paths < <(
  sed -e 's/[[:space:]]*$//' -e '/^[[:space:]]*$/d' -e '/^[[:space:]]*#/d' "$global_paths_file"
)

if [[ -n "$changed_files_file" ]]; then
  mapfile -t changed_files < "$changed_files_file"
else
  mapfile -t changed_files < <(git diff --name-only "$base...HEAD")
fi

if [[ "${#changed_files[@]}" -eq 0 ]]; then
  echo "affected-crates: no files changed vs ${base}; selecting nothing." >&2
  exit 0
fi

global_hits=()
for file in "${changed_files[@]}"; do
  [[ -z "$file" ]] && continue
  for prefix in "${global_paths[@]}"; do
    if [[ "$file" == "$prefix"* ]]; then
      global_hits+=("$file")
      break
    fi
  done
done

if [[ "${#global_hits[@]}" -gt 0 ]]; then
  preview="${global_hits[*]:0:5}"
  if [[ "${#global_hits[@]}" -gt 5 ]]; then
    preview="${preview} ..."
  fi
  echo "affected-crates: global/workspace-level change detected (${preview}); selecting the FULL workspace (no pruning)." >&2
  printf '%s\n' "--workspace"
  exit 0
fi

exec "$repo_root/scripts/harn_bin.sh" -- run scripts/affected-crates.harn -- --base "$base" --output args
