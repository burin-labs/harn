#!/usr/bin/env bash
# Restore or publish an immutable, copy-on-write Cargo target seed.
#
# Cargo target directories remain worktree-local. The seed is a completed
# target snapshot for one Rust/Cargo toolchain, cloned into a new lane without
# sharing mutable files. Cargo's own fingerprints invalidate branch-specific
# workspace artifacts while retaining the expensive compatible dependencies.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

usage() {
  echo "usage: $0 <restore|publish|key> [target-dir storage-root]" >&2
  exit 2
}

seed_key() {
  if [[ -n "${HARN_CARGO_TARGET_SEED_KEY:-}" ]]; then
    if [[ ! "${HARN_CARGO_TARGET_SEED_KEY}" =~ ^[A-Za-z0-9._-]+$ ]]; then
      echo "error: HARN_CARGO_TARGET_SEED_KEY must contain only letters, digits, dots, underscores, or hyphens" >&2
      return 2
    fi
    printf '%s\n' "${HARN_CARGO_TARGET_SEED_KEY}"
    return 0
  fi
  command -v rustc >/dev/null 2>&1 || return 1
  command -v cargo >/dev/null 2>&1 || return 1
  {
    printf 'harn-cargo-target-seed:v1\n'
    rustc -vV
    cargo -V
  } | shasum -a 256 | awk '{print $1}'
}

directory_is_empty() {
  [[ ! -d "$1" ]] || [[ -z "$(find "$1" -mindepth 1 -maxdepth 1 -print -quit)" ]]
}

cow_clone_supported() {
  local sample_file="$1"
  local probe_file="$2"
  local status=0

  case "$(uname -s)" in
    Darwin)
      cp -cP "${sample_file}" "${probe_file}" >/dev/null 2>&1 || status=$?
      ;;
    Linux)
      cp --reflink=always -- "${sample_file}" "${probe_file}" >/dev/null 2>&1 || status=$?
      ;;
    *)
      status=1
      ;;
  esac
  rm -f -- "${probe_file}"
  return "${status}"
}

copy_tree_cow() {
  local source="$1"
  local destination="$2"
  local sample_file probe_file

  # Unit tests need deterministic coverage on filesystems without reflinks.
  # Production has no byte-copy fallback: paying several GiB of I/O and disk
  # to make setup look warm defeats this module's resource contract.
  if [[ "${HARN_CARGO_TARGET_SEED_TEST_COPY:-0}" == "1" ]]; then
    mkdir -p "${destination}"
    cp -R "${source}/." "${destination}"
    return
  fi

  # Probe the source and destination filesystems before asking cp to walk a
  # multi-gigabyte target. Without this check, cp reports one reflink failure
  # per file before the caller can take the intended cold-build fallback.
  sample_file=""
  while IFS= read -r -d '' sample_file; do
    break
  done < <(find "${source}" -type f -print0)
  [[ -n "${sample_file}" ]] || return 1

  mkdir -p "$(dirname "${destination}")"
  probe_file="$(dirname "${destination}")/.harn-cargo-seed-probe-$$-${RANDOM}"
  cow_clone_supported "${sample_file}" "${probe_file}" || return 1

  case "$(uname -s)" in
    Darwin)
      mkdir -p "${destination}"
      cp -cRp "${source}/." "${destination}"
      ;;
    Linux)
      mkdir -p "${destination}"
      cp --reflink=always -a "${source}/." "${destination}/"
      ;;
    *)
      return 1
      ;;
  esac
}

clean_workspace_artifacts() {
  local target_dir="$1"

  # Cargo fingerprints and dep-info contain absolute paths to the worktree that
  # produced them. Keep third-party dependencies warm, but force every
  # workspace package to be rebuilt against the consuming worktree.
  (
    cd "${repo_root}"
    cargo clean --workspace --target-dir "${target_dir}" --locked >/dev/null
  )
}

cleanup_stage() {
  local stage="$1"
  local allowed_parent="$2"
  case "${stage}" in
    "${allowed_parent}"/.harn-cargo-seed-stage-*) rm -rf -- "${stage}" ;;
    *) echo "refusing to clean unexpected Cargo seed staging path: ${stage}" >&2 ;;
  esac
}

restore_seed() (
  local target_dir="$1"
  local storage_root="$2"
  local key seed_root seed_dir target_parent stage

  key="$(seed_key)" || {
    echo "Cargo target seed unavailable: Rust/Cargo toolchain identity could not be read."
    return 0
  }
  seed_root="${storage_root%/}/cargo-target-seed"
  seed_dir="${seed_root}/${key}"
  if [[ ! -f "${seed_dir}/.harn-cargo-target-seed" ]]; then
    echo "Cargo target seed cold: ${key}"
    return 0
  fi
  if ! directory_is_empty "${target_dir}"; then
    echo "Cargo target seed skipped: target is already populated."
    return 0
  fi

  target_parent="$(dirname "${target_dir}")"
  mkdir -p "${target_parent}"
  stage="${target_parent}/.harn-cargo-seed-stage-$$-${RANDOM}"
  trap '[[ -z "${stage:-}" ]] || cleanup_stage "${stage}" "${target_parent}"' EXIT
  if ! copy_tree_cow "${seed_dir}" "${stage}"; then
    cleanup_stage "${stage}" "${target_parent}"
    stage=""
    echo "Cargo target seed skipped: copy-on-write clones are unavailable on this filesystem."
    return 0
  fi
  if ! clean_workspace_artifacts "${stage}"; then
    cleanup_stage "${stage}" "${target_parent}"
    stage=""
    echo "Cargo target seed skipped: workspace artifacts could not be removed safely."
    return 0
  fi

  if ! directory_is_empty "${target_dir}"; then
    cleanup_stage "${stage}" "${target_parent}"
    stage=""
    echo "Cargo target seed skipped: another process populated the target."
    return 0
  fi
  if [[ -d "${target_dir}" ]] && ! rmdir "${target_dir}" 2>/dev/null; then
    cleanup_stage "${stage}" "${target_parent}"
    stage=""
    echo "Cargo target seed skipped: another process won the restore race."
    return 0
  fi
  if [[ -e "${target_dir}" ]]; then
    cleanup_stage "${stage}" "${target_parent}"
    stage=""
    echo "Cargo target seed skipped: another process won the restore race."
    return 0
  fi
  if mv "${stage}" "${target_dir}" 2>/dev/null; then
    stage=""
    echo "Restored Cargo target seed -> ${target_dir}"
  else
    cleanup_stage "${stage}" "${target_parent}"
    stage=""
    echo "Cargo target seed skipped: another process won the restore race."
  fi
)

publish_seed() (
  local target_dir="$1"
  local storage_root="$2"
  local key seed_root seed_dir lock_dir stage max_kib target_kib

  [[ -d "${target_dir}" ]] || return 0
  key="$(seed_key)" || return 0
  seed_root="${storage_root%/}/cargo-target-seed"
  seed_dir="${seed_root}/${key}"
  mkdir -p "${seed_root}"
  if [[ -f "${seed_dir}/.harn-cargo-target-seed" ]]; then
    echo "Cargo target seed already published: ${key}"
    return 0
  fi

  # A seed must accelerate a fresh lane without pinning an indefinitely grown
  # developer target. The canonical CLI target is about 5 GiB; 8 GiB leaves
  # headroom while rejecting multi-profile targets observed above 80 GiB.
  max_kib="${HARN_CARGO_TARGET_SEED_MAX_KIB:-8388608}"
  if [[ ! "${max_kib}" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: HARN_CARGO_TARGET_SEED_MAX_KIB must be a positive integer" >&2
    return 2
  fi
  target_kib="$(du -sk "${target_dir}" | awk '{print $1}')" || return 0
  if [[ ! "${target_kib}" =~ ^[0-9]+$ ]] || (( target_kib > max_kib )); then
    echo "Cargo target seed publish skipped: target size ${target_kib:-unknown} KiB exceeds the ${max_kib} KiB ceiling."
    return 0
  fi

  lock_dir="${seed_root}/.publish-${key}"
  if ! mkdir "${lock_dir}" 2>/dev/null; then
    echo "Cargo target seed publish skipped: another publisher owns ${key}."
    return 0
  fi
  stage="${seed_root}/.harn-cargo-seed-stage-$$-${RANDOM}"
  trap '
    [[ -z "${stage:-}" ]] || cleanup_stage "${stage}" "${seed_root}"
    [[ -z "${lock_dir:-}" ]] || rmdir "${lock_dir}" 2>/dev/null || true
  ' EXIT
  if ! copy_tree_cow "${target_dir}" "${stage}"; then
    cleanup_stage "${stage}" "${seed_root}"
    stage=""
    rmdir "${lock_dir}" 2>/dev/null || true
    lock_dir=""
    echo "Cargo target seed publish skipped: copy-on-write clones are unavailable on this filesystem."
    return 0
  fi
  if ! clean_workspace_artifacts "${stage}"; then
    cleanup_stage "${stage}" "${seed_root}"
    stage=""
    rmdir "${lock_dir}" 2>/dev/null || true
    lock_dir=""
    echo "Cargo target seed publish skipped: workspace artifacts could not be removed safely."
    return 0
  fi
  printf '%s\n' "${key}" > "${stage}/.harn-cargo-target-seed"
  if [[ ! -e "${seed_dir}" ]]; then
    mv "${stage}" "${seed_dir}"
    stage=""
    echo "Published Cargo target seed -> ${seed_dir}"
  else
    cleanup_stage "${stage}" "${seed_root}"
    stage=""
    echo "Cargo target seed publish skipped: another publisher completed first."
  fi
  rmdir "${lock_dir}" 2>/dev/null || true
  lock_dir=""
)

action="${1:-}"
case "${action}" in
  key)
    [[ "$#" -eq 1 ]] || usage
    seed_key
    ;;
  restore | publish)
    [[ "$#" -eq 3 ]] || usage
    if [[ "${action}" == "restore" ]]; then
      restore_seed "$2" "$3"
    else
      publish_seed "$2" "$3"
    fi
    ;;
  *) usage ;;
esac
