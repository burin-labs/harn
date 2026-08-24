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

retry_toolchain_command rustc -Vv
retry_toolchain_command cargo -V
