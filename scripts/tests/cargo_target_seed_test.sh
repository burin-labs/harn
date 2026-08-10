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

echo "cargo_target_seed_test: ok"
