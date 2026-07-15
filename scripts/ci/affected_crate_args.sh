#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
global_paths_file="$repo_root/scripts/config/affected-crate-global-paths.txt"
base="origin/main"
changed_files_file=""

usage() {
  cat <<'EOF'
usage: scripts/ci/affected_crate_args.sh [--base REF] [--changed-files-file PATH]

Print cargo-nextest package args for the PR affected-crate test lane without
building the Harn CLI. This mirrors scripts/affected-crates.harn for CI
bootstrap, where invoking Harn just to choose Rust packages would compile the
runtime before nextest can start.
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

global_paths=()
while IFS= read -r line; do
  global_paths+=("$line")
done < <(
  sed -e 's/[[:space:]]*$//' -e '/^[[:space:]]*$/d' -e '/^[[:space:]]*#/d' "$global_paths_file"
)

changed_files=()
if [[ -n "$changed_files_file" ]]; then
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    changed_files+=("$file")
  done < "$changed_files_file"
else
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    changed_files+=("$file")
  done < <(git diff --name-only "$base...HEAD")
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

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required for buildless affected-crate selection" >&2
  exit 1
fi

workspace_root="$(git rev-parse --show-toplevel)"
metadata="$(cargo metadata --no-deps --format-version 1)"

sort_unique() {
  if [[ $# -eq 0 ]]; then
    return 0
  fi
  printf '%s\n' "$@" | sed '/^$/d' | LC_ALL=C sort -u
}

contains() {
  local needle="$1"
  shift
  local item
  for item in "$@"; do
    [[ "$item" == "$needle" ]] && return 0
  done
  return 1
}

package_names=()
package_dirs=()
package_deps=()
while IFS=$'\t' read -r name manifest_path deps; do
  [[ -z "$name" || -z "$manifest_path" ]] && continue
  dir="${manifest_path%/Cargo.toml}"
  if [[ "$dir" == "$workspace_root" ]]; then
    rel_dir="."
  else
    rel_dir="${dir#"$workspace_root"/}"
  fi
  package_names+=("$name")
  package_dirs+=("$rel_dir")
  package_deps+=("$deps")
done < <(
  jq -r '
    . as $meta
    | .packages[]
    | select(.id as $id | $meta.workspace_members | index($id))
    | [
        .name,
        .manifest_path,
        ((.dependencies // []) | map(.name) | unique | join(","))
      ]
    | @tsv
  ' <<<"$metadata"
)

selected=()
directly_changed=()
unowned=()

add_selected() {
  contains "$1" "${selected[@]}" && return 0
  selected+=("$1")
}

add_directly_changed() {
  contains "$1" "${directly_changed[@]}" && return 0
  directly_changed+=("$1")
}

add_unowned() {
  contains "$1" "${unowned[@]}" && return 0
  unowned+=("$1")
}

for file in "${changed_files[@]}"; do
  best_name=""
  best_len=-1
  for ((i = 0; i < ${#package_names[@]}; i++)); do
    dir="${package_dirs[$i]}"
    if [[ "$file" == "$dir" || "$file" == "$dir/"* ]]; then
      if (( ${#dir} > best_len )); then
        best_name="${package_names[$i]}"
        best_len=${#dir}
      fi
    fi
  done
  if [[ -z "$best_name" ]]; then
    add_unowned "$file"
  else
    add_directly_changed "$best_name"
    add_selected "$best_name"
  fi
done

if [[ "${#selected[@]}" -eq 0 ]]; then
  preview="${unowned[*]:0:3}"
  if [[ "${#unowned[@]}" -gt 3 ]]; then
    preview="${preview} ..."
  fi
  echo "affected-crates: changed files touch no Rust crate (e.g. ${preview}); selecting nothing." >&2
  exit 0
fi

changed=1
while [[ "$changed" -eq 1 ]]; do
  changed=0
  for ((i = 0; i < ${#package_names[@]}; i++)); do
    name="${package_names[$i]}"
    contains "$name" "${selected[@]}" && continue
    IFS=',' read -r -a deps <<<"${package_deps[$i]}"
    for dep in "${deps[@]}"; do
      [[ -z "$dep" ]] && continue
      if contains "$dep" "${selected[@]}"; then
        selected+=("$name")
        changed=1
        break
      fi
    done
  done
done

selected_sorted=()
while IFS= read -r item; do
  [[ -z "$item" ]] && continue
  selected_sorted+=("$item")
done < <(sort_unique "${selected[@]}")

directly_sorted=()
while IFS= read -r item; do
  [[ -z "$item" ]] && continue
  directly_sorted+=("$item")
done < <(sort_unique "${directly_changed[@]}")

pruned=()
for name in "${package_names[@]}"; do
  contains "$name" "${selected_sorted[@]}" || pruned+=("$name")
done
pruned_sorted=()
while IFS= read -r item; do
  [[ -z "$item" ]] && continue
  pruned_sorted+=("$item")
done < <(sort_unique "${pruned[@]}")

echo "affected-crates: directly changed: ${directly_sorted[*]}" >&2
echo "affected-crates: selected (changed + rdeps closure): ${selected_sorted[*]}" >&2
if [[ "${#pruned_sorted[@]}" -eq 0 ]]; then
  echo "affected-crates: pruned (not selected): (none)" >&2
else
  echo "affected-crates: pruned (not selected): ${pruned_sorted[*]}" >&2
fi
if [[ "${#unowned[@]}" -gt 0 ]]; then
  echo "affected-crates: note: non-crate files also changed (not test-relevant at the crate level): ${unowned[*]:0:8}" >&2
fi

all_sorted=()
while IFS= read -r item; do
  [[ -z "$item" ]] && continue
  all_sorted+=("$item")
done < <(sort_unique "${package_names[@]}")

if [[ "${#selected_sorted[@]}" -eq "${#all_sorted[@]}" ]]; then
  full=1
  for ((i = 0; i < ${#all_sorted[@]}; i++)); do
    if [[ "${selected_sorted[$i]}" != "${all_sorted[$i]}" ]]; then
      full=0
      break
    fi
  done
  if [[ "$full" -eq 1 ]]; then
    echo "--workspace"
    exit 0
  fi
fi

rendered=()
for name in "${selected_sorted[@]}"; do
  rendered+=("-p $name")
done
printf '%s\n' "${rendered[*]}"
