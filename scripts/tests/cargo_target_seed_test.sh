#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
seed_tool="${repo_root}/scripts/cargo_target_seed.sh"
tmp_root="$(mktemp -d)"
trap 'rm -rf "${tmp_root}"' EXIT

source_target="${tmp_root}/source target"
restored_target="${tmp_root}/restored target"
occupied_target="${tmp_root}/occupied target"
storage_root="${tmp_root}/storage root"
fake_bin="${tmp_root}/fake bin"
mkdir -p \
  "${source_target}/debug/deps" \
  "${source_target}/debug/.fingerprint/harn-vm-test" \
  "${restored_target}" \
  "${occupied_target}" \
  "${fake_bin}"
# The single-quoted lines are the fake command's source, not this test's vars.
# shellcheck disable=SC2016
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  '[[ "$1" == "clean" && "$2" == "--workspace" ]]' \
  'while [[ "$#" -gt 0 ]]; do' \
  '  if [[ "$1" == "--target-dir" ]]; then target_dir="$2"; shift 2; continue; fi' \
  '  shift' \
  'done' \
  '[[ -n "${target_dir:-}" ]]' \
  'rm -f -- "${target_dir}/debug/harn" "${target_dir}/debug/harn.d"' \
  'rm -f -- "${target_dir}/debug/deps/libharn_vm-test.rlib"' \
  'rm -rf -- "${target_dir}/debug/.fingerprint/harn-vm-test"' \
  > "${fake_bin}/cargo"
chmod +x "${fake_bin}/cargo"
printf '%s\n' '#!/usr/bin/env bash' > "${source_target}/debug/harn"
chmod +x "${source_target}/debug/harn"
printf '%s\n' 'publisher worktree' > "${source_target}/debug/harn.d"
printf '%s\n' 'publisher fingerprint' \
  > "${source_target}/debug/.fingerprint/harn-vm-test/invoked.timestamp"
printf '%s\n' 'publisher workspace artifact' \
  > "${source_target}/debug/deps/libharn_vm-test.rlib"
printf '%s\n' 'reusable dependency artifact' > "${source_target}/debug/deps/libreusable.rlib"
printf '%s\n' 'operator-owned' > "${occupied_target}/sentinel"

seed_env=(
  HARN_CARGO_TARGET_SEED_KEY=test-toolchain
  HARN_CARGO_TARGET_SEED_TEST_COPY=1
  PATH="${fake_bin}:${PATH}"
)
env "${seed_env[@]}" "${seed_tool}" publish "${source_target}" "${storage_root}"
seed_dir="${storage_root}/cargo-target-seed/test-toolchain"
if [[ ! -f "${seed_dir}/.harn-cargo-target-seed" \
  || ! -f "${seed_dir}/debug/deps/libreusable.rlib" \
  || -e "${seed_dir}/debug/harn" \
  || -e "${seed_dir}/debug/harn.d" \
  || -e "${seed_dir}/debug/deps/libharn_vm-test.rlib" \
  || -e "${seed_dir}/debug/.fingerprint/harn-vm-test" ]]; then
  echo "publish did not create a complete immutable Cargo target seed" >&2
  exit 1
fi

env "${seed_env[@]}" "${seed_tool}" restore "${restored_target}" "${storage_root}"
if [[ ! -f "${restored_target}/debug/deps/libreusable.rlib" \
  || -e "${restored_target}/debug/harn" \
  || -e "${restored_target}/debug/harn.d" \
  || -e "${restored_target}/debug/deps/libharn_vm-test.rlib" \
  || -e "${restored_target}/debug/.fingerprint/harn-vm-test" ]]; then
  echo "restore did not project the seed into the empty target" >&2
  exit 1
fi
# A restored tree must carry this tree's age, not the seed's. The copy
# reproduces the seed's timestamps exactly, so before this was stamped a target
# restored seconds ago reported an mtime from whenever the seed was published
# and the target-cache GC retired it as a cold cache before its first build.
# Backdate the seed itself so the restore has an old age to inherit, then read
# the restored tree's age back.
touch -t 202001010000 \
  "${seed_dir}" "${seed_dir}/debug" "${seed_dir}/debug/deps/libreusable.rlib"
stamped_target="${tmp_root}/stamped target"
mkdir -p "${stamped_target}"
env "${seed_env[@]}" "${seed_tool}" restore "${stamped_target}" "${storage_root}" >/dev/null
restore_epoch="$(date +%s)"
for stamped in "${stamped_target}" "${stamped_target}/debug"; do
  stamped_epoch="$(stat -c %Y "${stamped}" 2>/dev/null || stat -f %m "${stamped}" 2>/dev/null)"
  if [[ -z "${stamped_epoch}" ]] || (( restore_epoch - stamped_epoch > 600 )); then
    echo "a just-restored tree reported the seed's age at ${stamped}: ${stamped_epoch:-unreadable}" >&2
    exit 1
  fi
done
# The control that keeps the fix honest. Stamping the artifacts too would be an
# easy way to pass the assertion above and would destroy the reuse the seed
# exists for, because Cargo decides freshness by comparing these mtimes against
# source files. Only the directories the GC reads may move.
artifact_epoch="$(
  stat -c %Y "${stamped_target}/debug/deps/libreusable.rlib" 2>/dev/null \
    || stat -f %m "${stamped_target}/debug/deps/libreusable.rlib" 2>/dev/null
)"
if [[ -z "${artifact_epoch}" ]] || (( restore_epoch - artifact_epoch < 600 )); then
  echo "the restore touched a seed artifact, which invalidates Cargo's freshness reuse" >&2
  exit 1
fi

printf '%s\n' 'lane-local mutation' > "${restored_target}/debug/deps/libreusable.rlib"
if ! grep -Fxq 'reusable dependency artifact' "${seed_dir}/debug/deps/libreusable.rlib"; then
  echo "a restored lane mutated the immutable seed" >&2
  exit 1
fi

occupied_output="$(
  env "${seed_env[@]}" "${seed_tool}" restore "${occupied_target}" "${storage_root}"
)"
if ! grep -Fq 'target is already populated' <<< "${occupied_output}" \
  || ! grep -Fxq 'operator-owned' "${occupied_target}/sentinel"; then
  echo "restore overwrote an existing target directory" >&2
  exit 1
fi

env "${seed_env[@]}" "${seed_tool}" publish "${restored_target}" "${storage_root}" >/dev/null
if [[ "$(find "${storage_root}/cargo-target-seed" -mindepth 1 -maxdepth 1 -type d ! -name '.publish-*' | wc -l | tr -d ' ')" != "1" ]]; then
  echo "idempotent publish created a parallel seed" >&2
  exit 1
fi

oversized_output="$(
  HARN_CARGO_TARGET_SEED_KEY=oversized-toolchain \
  HARN_CARGO_TARGET_SEED_TEST_COPY=1 \
  HARN_CARGO_TARGET_SEED_MAX_KIB=1 \
    "${seed_tool}" publish "${source_target}" "${storage_root}"
)"
if ! grep -Fq 'exceeds the 1 KiB ceiling' <<< "${oversized_output}" \
  || [[ -e "${storage_root}/cargo-target-seed/oversized-toolchain" ]]; then
  echo "publish retained an oversized Cargo target seed" >&2
  exit 1
fi

unavailable_bin="${tmp_root}/unavailable bin"
mkdir -p "${unavailable_bin}"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'echo "simulated reflink error" >&2' \
  'exit 1' \
  > "${unavailable_bin}/cp"
chmod +x "${unavailable_bin}/cp"
unavailable_output="$(
  HARN_CARGO_TARGET_SEED_KEY=unavailable-toolchain \
  PATH="${unavailable_bin}:${fake_bin}:${PATH}" \
    "${seed_tool}" publish "${source_target}" "${storage_root}" 2>&1
)"
if [[ "$(wc -l <<< "${unavailable_output}" | tr -d ' ')" != "1" ]] \
  || ! grep -Fq 'copy-on-write clones are unavailable' <<< "${unavailable_output}" \
  || grep -Fq 'simulated reflink error' <<< "${unavailable_output}" \
  || [[ -e "${storage_root}/cargo-target-seed/unavailable-toolchain" ]] \
  || find "${storage_root}/cargo-target-seed" -maxdepth 1 -name '.harn-cargo-seed-stage-*' -print -quit | grep -q .; then
  echo "unavailable copy-on-write support did not take the quiet cold-build fallback" >&2
  exit 1
fi

echo "cargo_target_seed_test: ok"
