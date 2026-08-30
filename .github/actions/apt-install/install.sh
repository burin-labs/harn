#!/usr/bin/env bash
set -euo pipefail

# Apt's retry count does not bound an attempt that never returns. Keep every
# network operation below the composite action's five-minute backstop so the
# fallback path can actually run when a hosted-runner mirror stalls.
readonly APT_UPDATE_SECONDS="${APT_UPDATE_SECONDS:-40}"
readonly APT_INSTALL_SECONDS="${APT_INSTALL_SECONDS:-60}"
readonly APT_NETWORK_SECONDS="${APT_NETWORK_SECONDS:-15}"
readonly APT_SOURCES_DIR="${APT_SOURCES_DIR:-/etc/apt/sources.list.d}"

check_commands=()
while IFS= read -r command_name; do
  check_commands+=("$command_name")
done < <(printf '%s\n' "${CHECK_COMMAND:-}" | tr '[:space:]' '\n' | sed '/^$/d')
if [[ "${#check_commands[@]}" -gt 0 ]]; then
  all_present=true
  for command_name in "${check_commands[@]}"; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
      all_present=false
      break
    fi
  done
  if [[ "$all_present" == "true" ]]; then
    for command_name in "${check_commands[@]}"; do
      "$command_name" --version || true
    done
    exit 0
  fi
fi

packages=()
while IFS= read -r package_name; do
  packages+=("$package_name")
done < <(printf '%s\n' "${APT_PACKAGES:-}" | tr '[:space:]' '\n' | sed '/^$/d')
if [[ "${#packages[@]}" -eq 0 ]]; then
  echo "::error::apt-install requires at least one package"
  exit 1
fi

run_apt() {
  local limit_seconds="$1"
  shift
  sudo timeout --signal=TERM --kill-after=5s "${limit_seconds}s" \
    apt-get \
    -o "Acquire::Retries=2" \
    -o "Acquire::http::Timeout=${APT_NETWORK_SECONDS}" \
    -o "Acquire::https::Timeout=${APT_NETWORK_SECONDS}" \
    "$@"
}

apt_update() {
  run_apt "$APT_UPDATE_SECONDS" update
}

apt_install() {
  run_apt "$APT_INSTALL_SECONDS" install -y --no-install-recommends "${packages[@]}"
}

# Hosted images normally have usable package indexes already. Installing first
# avoids turning a healthy package cache into a dependency on every configured
# mirror. Refresh only when apt proves the cache is insufficient.
if ! apt_install; then
  echo "::warning::apt-get install failed or timed out; refreshing package metadata before retrying"
  if ! apt_update; then
    echo "::warning::apt-get update failed or timed out; disabling hosted-runner Microsoft apt sources and retrying"
    disabled_dir="${RUNNER_TEMP:-/tmp}/disabled-apt-sources"
    sudo mkdir -p "$disabled_dir"
    shopt -s nullglob
    for source_file in "$APT_SOURCES_DIR"/*; do
      if sudo grep -qi 'packages.microsoft.com' "$source_file"; then
        sudo mv "$source_file" "$disabled_dir/$(basename "$source_file")"
      fi
    done
    apt_update
  fi
  apt_install
fi

if [[ "${#check_commands[@]}" -gt 0 ]]; then
  for command_name in "${check_commands[@]}"; do
    command -v "$command_name"
    "$command_name" --version || true
  done
fi
