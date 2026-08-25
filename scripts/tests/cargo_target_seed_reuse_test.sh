#!/usr/bin/env bash
set -euo pipefail

: "${HARN_BIN:?run this integration through make test-pr-gate-post-warm-integrations}"

# Cargo's verbose status is the assertion surface below. Keep it stable when
# CI sets CARGO_TERM_COLOR=always.
export CARGO_TERM_COLOR=never

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
seed_tool="${repo_root}/scripts/cargo_target_seed.sh"
tmp_root="$(mktemp -d)"
trap 'rm -rf "${tmp_root}"' EXIT

source_workspace="${tmp_root}/publisher"
consumer_workspace="${tmp_root}/consumer"
shared_dependency="${tmp_root}/shared dependency"
source_target="${tmp_root}/publisher target"
consumer_target="${tmp_root}/consumer target"
storage_root="${tmp_root}/storage"
consumer_log="${tmp_root}/consumer build.log"
mkdir -p "${source_workspace}/src" "${consumer_workspace}/src" "${shared_dependency}/src"
printf '%s\n' \
  '[package]' \
  'name = "harn-seed-reuse-dependency"' \
  'version = "0.0.0"' \
  'edition = "2021"' \
  '' \
  '[workspace]' \
  > "${shared_dependency}/Cargo.toml"
printf '%s\n' 'pub fn probe() -> u32 { 7236 }' > "${shared_dependency}/src/lib.rs"
printf '%s\n' \
  '[package]' \
  'name = "harn-seed-reuse-probe"' \
  'version = "0.0.0"' \
  'edition = "2021"' \
  '' \
  '[dependencies]' \
  'harn-seed-reuse-dependency = { path = "../shared dependency" }' \
  '' \
  '[workspace]' \
  > "${source_workspace}/Cargo.toml"
printf '%s\n' \
  'fn main() {' \
  '    assert_eq!(harn_seed_reuse_dependency::probe(), 7236);' \
  '}' \
  > "${source_workspace}/src/main.rs"
cp -R "${source_workspace}/." "${consumer_workspace}"

HARN_ALLOW_RAW_CARGO=1 cargo build \
  --manifest-path "${source_workspace}/Cargo.toml" \
  --target-dir "${source_target}" \
  --offline \
  --verbose \
  >/dev/null 2>&1
cp "${source_workspace}/Cargo.lock" "${consumer_workspace}/Cargo.lock"
HARN_ALLOW_RAW_CARGO=1 cargo clean \
  --manifest-path "${source_workspace}/Cargo.toml" \
  --target-dir "${source_target}" \
  --package harn-seed-reuse-probe \
  >/dev/null 2>&1

seed_env=(
  HARN_CARGO_TARGET_SEED_KEY=real-toolchain
  HARN_CARGO_TARGET_SEED_TEST_COPY=1
)
env "${seed_env[@]}" \
  "${seed_tool}" publish "${source_target}" "${storage_root}" >/dev/null 2>&1
env "${seed_env[@]}" \
  "${seed_tool}" restore "${consumer_target}" "${storage_root}" >/dev/null 2>&1
HARN_ALLOW_RAW_CARGO=1 cargo build \
  --manifest-path "${consumer_workspace}/Cargo.toml" \
  --target-dir "${consumer_target}" \
  --locked \
  --offline \
  --verbose \
  >"${consumer_log}" 2>&1

if ! grep -Eq 'Fresh harn-seed-reuse-dependency v' "${consumer_log}" \
  || grep -Eq 'Compiling harn-seed-reuse-dependency v' "${consumer_log}" \
  || ! grep -Eq 'Compiling harn-seed-reuse-probe v' "${consumer_log}"; then
  echo "restored Cargo seed did not reuse dependencies across workspace paths" >&2
  sed -n '/Fresh\|Compiling/p' "${consumer_log}" >&2
  exit 1
fi

echo "Cargo target seed cross-path reuse: 1/1 dependency fresh; workspace crate rebuilt."
echo "cargo_target_seed_reuse_test: ok"
