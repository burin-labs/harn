#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <package-directory>" >&2
  exit 2
}

[[ "$#" -eq 1 ]] || usage
package_dir="$1"
[[ -d "$package_dir" ]] || {
  echo "error: npm package directory does not exist: $package_dir" >&2
  exit 2
}

log_file="$(mktemp "${TMPDIR:-/tmp}/harn-npm-ci.XXXXXX")"
trap 'rm -f "$log_file"' EXIT

retry_delays=(5 20 60)
attempt=1
while :; do
  : >"$log_file"
  set +e
  (cd "$package_dir" && npm ci) > >(tee "$log_file") 2>&1
  status=$?
  set -e
  [[ "$status" -eq 0 ]] && exit 0

  # Package/script failures are deterministic and must fail immediately. Retry
  # only transport diagnostics emitted while npm or an install hook downloads
  # immutable dependencies. Three bounded, increasing waits cover a sustained
  # release-asset edge reset without turning a persistent outage into a loop.
  if [[ "$attempt" -gt "${#retry_delays[@]}" ]] || ! grep -Eiq \
    'ECONNRESET|socket hang up|ETIMEDOUT|EAI_AGAIN|ENETUNREACH|ECONNREFUSED|HTTP (429|5[0-9][0-9])' \
    "$log_file"; then
    exit "$status"
  fi

  delay="${retry_delays[$((attempt - 1))]}"
  echo "warning: npm ci hit a transient network failure; retrying in ${delay}s (attempt $((attempt + 1))/4)" >&2
  sleep "$delay"
  attempt=$((attempt + 1))
done
