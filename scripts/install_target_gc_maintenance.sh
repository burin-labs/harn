#!/usr/bin/env bash
# Install one daily host job. The job's wrapper fetches the current collector;
# no long-lived worktree or checkout-local setup needs to stay up to date.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
installed="${HARN_TARGET_GC_INSTALL_DIR:-$HOME/.local/bin}/harn-target-gc-maintenance"
log_dir="${HARN_TARGET_GC_LOG_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/harn}"
marker="# harn-target-gc-maintenance"

mkdir -p "$(dirname "$installed")" "$log_dir"
install -m 0755 "$script_dir/target_gc_maintenance.sh" "$installed"

if ! existing="$(crontab -l 2>&1)"; then
  case "$existing" in
    *"no crontab for"*) existing="" ;;
    *) echo "harn-target maintenance: cannot read existing crontab: $existing" >&2; exit 1 ;;
  esac
fi
without_old="$(printf '%s\n' "$existing" | awk -v marker="$marker" 'index($0, marker) == 0')"
entry="17 3 * * * $installed >> $log_dir/target-gc-maintenance.log 2>&1 $marker"
printf '%s\n%s\n' "$without_old" "$entry" | crontab -
installed_tab="$(crontab -l)"
if [ "$(printf '%s\n' "$installed_tab" | grep -Fc "$marker")" -ne 1 ] \
  || ! grep -Fq "$entry" <<< "$installed_tab"; then
  echo "harn-target maintenance: daily job did not read back" >&2
  exit 1
fi
echo "harn-target maintenance: installed daily job and current-policy wrapper"
