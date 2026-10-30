#!/usr/bin/env bash
set -euo pipefail

retry_toolchain_command() {
  local attempt=1
  local max_attempts=4
  local delay=2
  until "$@"; do
    if (( attempt >= max_attempts )); then
      echo "Rust toolchain command failed after ${attempt} attempts: $*" >&2
      return 1
    fi
    echo "::warning::Rust toolchain transport failed (attempt ${attempt}/${max_attempts}); retrying in ${delay}s: $*"
    sleep "$delay"
    attempt=$((attempt + 1))
    delay=$((delay * 2))
  done
}

# rust-toolchain.toml owns the exact channel. Any rustup proxy can trigger the
# pinned toolchain download, so keep both explicit rustup operations and the
# final rustc/cargo probes inside the same bounded retry policy.
retry_toolchain_command rustup show

components=()
while IFS= read -r component; do
  components+=("$component")
done < <(printf '%s\n' "${EXTRA_COMPONENTS:-}" | tr ',' '\n' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//;/^$/d')
if [[ "${#components[@]}" -gt 0 ]]; then
  retry_toolchain_command rustup component add "${components[@]}"
fi

targets=()
while IFS= read -r target; do
  targets+=("$target")
done < <(printf '%s\n' "${EXTRA_TARGETS:-}" | tr ',' '\n' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//;/^$/d')
if [[ "${#targets[@]}" -gt 0 ]]; then
  retry_toolchain_command rustup target add "${targets[@]}"
fi

# The Rust cache key hashes every installed rustup toolchain, not just the
# one this repository builds with. GitHub's images preinstall a floating
# `stable`, and during an image rollout two runners in one run report different
# ones (1.98.1 and 1.99.0 on 2026-10-06), so a cache saved on one image is
# invisible to a job on the other: its key prefix differs and the restore finds
# nothing (#9430). A hosted runner is discarded after the job, so it keeps only
# the pinned toolchain and makes it the default, which also gives cargo run
# outside the checkout the same compiler instead of the image's. Owned runners
# share one rustup home across jobs and are left alone.
if [[ "${RUNNER_ENVIRONMENT:-}" == github-hosted ]]; then
  active="$(rustup show active-toolchain | awk '{print $1}')"
  if [[ -z "$active" ]]; then
    echo "::error::could not read the active Rust toolchain"
    exit 1
  fi
  retry_toolchain_command rustup default "$active"
  while IFS= read -r toolchain; do
    [[ -n "$toolchain" && "$toolchain" != "$active" ]] || continue
    rustup toolchain uninstall "$toolchain"
  done < <(rustup toolchain list --quiet | awk '{print $1}')
  echo "Rust toolchains kept on this hosted runner: $(rustup toolchain list --quiet | tr '\n' ' ')"
fi

retry_toolchain_command rustc -Vv
retry_toolchain_command cargo -V

# Every later step in the job compiles Harn's crates with rustc's parallel
# front end through the workspace wrapper (scripts/lib/rustc_frontend.sh says
# why). The library keeps Windows jobs serial.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
# shellcheck source=scripts/lib/rustc_frontend.sh
source "${repo_root}/scripts/lib/rustc_frontend.sh"
frontend_flags="$(harn_rustc_frontend_flags)"
if [[ -n "$frontend_flags" ]]; then
  wrapper="$(harn_rustc_frontend_install_wrapper "${repo_root}/scripts" \
    "${RUNNER_TEMP:?the wrapper copy lives in the runner temp directory}/harn-rustc-frontend")"
  printf 'RUSTC_WORKSPACE_WRAPPER=%s\n' "${wrapper}" \
    >> "${GITHUB_ENV:?the workspace wrapper reaches later steps only through GITHUB_ENV}"
  echo "rustc front end: ${frontend_flags} for workspace crates"
else
  echo "rustc front end: serial for workspace crates"
fi
