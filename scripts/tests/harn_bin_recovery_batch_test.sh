#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# The path is repository-relative but resolved dynamically for fixture use.
# shellcheck disable=SC1091
source "$repo_root/scripts/lib/harn_bin.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT
target_dir="$tmp_root/target"
binary="$target_dir/debug/harn"
checker="$target_dir/debug/harn-freshness-check"
count_file="$tmp_root/fingerprint-count"
mkdir -p "$(dirname "$binary")"
printf '#!/usr/bin/env bash\n' > "$binary"
printf '#!/usr/bin/env bash\n' > "$checker"
chmod +x "$binary" "$checker"
: > "$count_file"

harn_retry_without_wrapper() { printf '0\n'; }
harn_effective_cargo_lease_mode() { printf 'off\n'; }
harn_export_cargo_build_dir_for_target() { :; }
harn_debug_binary_path() { printf '%s\n' "$binary"; }
harn_cargo_freshness_checker_path() { printf '%s\n' "$checker"; }
harn_require_binary_freshness_receipt() { return 1; }
harn_worktree_unconfigured() { return 1; }
harn_build_freshness_id() {
  printf 'probe=%s\n' "${2:-0}" >> "$count_file"
  # Model the disappearing generated dependency: strict artifact inspection
  # cannot read the old dep-info graph, while recovery mode returns bootstrap.
  if [[ "${2:-0}" = '1' ]] && [[ "$(wc -l < "$count_file")" -eq 1 ]]; then
    return 1
  fi
  printf '%064d\n' 0
}
harn_run_cargo_probe_with_deadline() {
  printf '#!/usr/bin/env bash\n' > "$binary"
  chmod +x "$binary"
  printf '%s\n' "$binary"
}
harn_shell_executable_path() { printf '%s\n' "$1"; }
harn_require_executable_bin() { [[ -x "$1" ]]; }
harn_embedded_build_freshness_id() { printf '%064d\n' 0; }
harn_record_binary_freshness() { :; }

CARGO_TARGET_DIR="$target_dir" HARN_CARGO_LEASE_MODE=off \
  harn_resolve_binary build >/dev/null

if [[ "$(wc -l < "$count_file" | tr -d ' ')" -ne 2 ]] || \
   ! grep -Fxq 'probe=0' "$count_file"; then
  echo 'build-mode recovery recomputed the worktree freshness fingerprint' >&2
  cat "$count_file" >&2
  exit 1
fi

echo 'harn_bin_recovery_batch_test: ok'
