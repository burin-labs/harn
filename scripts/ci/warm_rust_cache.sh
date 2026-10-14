#!/usr/bin/env bash
# Warm the shared Linux workspace-tests Cargo graph on refs/heads/main.
#
# The scheduled cache writer refreshes the same graph as main's test producer
# between pushes, without re-running the suite.
#
# Pair with cache-workspace-crates=true on the writer: Swatinem otherwise
# strips workspace artifacts before save and merge_group still rebuilds every
# harn-* crate after an exact key hit (#5003).
set -euo pipefail

# Keep the compiler foregrounded and preserve its exit status. A runner that
# disappears mid-compile leaves a begin record, never a successful end record.
if ! /usr/bin/time --version 2>/dev/null | grep -q 'GNU Time'; then
  echo 'workspace warm: GNU time is required for measured compiler phases' >&2
  exit 1
fi
phase_metrics="$(mktemp "${TMPDIR:-/tmp}/harn-workspace-warm.XXXXXXXX")"
trap 'rm -f -- "$phase_metrics"' EXIT
warm_phase() {
  local phase="$1" compiler_status=0 elapsed peak_rss measured_status
  shift
  local disk_kib memory_kib
  disk_kib="$(df -Pk . | awk 'NR == 2 {print $4}')"
  memory_kib="$(awk '$1 == "MemAvailable:" {print $2}' /proc/meminfo)"
  [[ "$disk_kib" =~ ^[0-9]+$ && "$memory_kib" =~ ^[0-9]+$ ]] || {
    echo "workspace warm: $phase resource read is unmeasured" >&2
    return 1
  }
  printf 'workspace warm phase=%s state=begin disk_available_kib=%s memory_available_kib=%s\n' "$phase" "$disk_kib" "$memory_kib"
  : > "$phase_metrics"
  /usr/bin/time -q -f '%e %M %x' -o "$phase_metrics" -- "$@" || compiler_status=$?
  read -r elapsed peak_rss measured_status < "$phase_metrics" || {
    echo "workspace warm: $phase compiler metrics are unmeasured" >&2
    [[ "$compiler_status" != 0 ]] && return "$compiler_status"
    return 1
  }
  if [[ ! "$elapsed" =~ ^[0-9]+([.][0-9]+)?$ || ! "$peak_rss" =~ ^[0-9]+$ || "$measured_status" != "$compiler_status" ]]; then
    echo "workspace warm: $phase compiler metrics are invalid" >&2
    [[ "$compiler_status" != 0 ]] && return "$compiler_status"
    return 1
  fi
  printf 'workspace warm phase=%s state=end compiler_exit=%s elapsed_seconds=%s peak_rss_kib=%s\n' "$phase" "$compiler_status" "$elapsed" "$peak_rss"
  return "$compiler_status"
}

warm_phase harn-build cargo build --locked --bin harn
host_bound_filter="$(scripts/ci/host_bound_rust_test_filter.sh)"
warm_phase workspace-tests cargo-nextest nextest run --locked --workspace --profile ci --no-run \
  -E "not (${host_bound_filter})"
# Match rust-check-inputs' exact GitHub-owned security archive compile shape.
warm_phase security-tests cargo-nextest nextest run --locked --workspace --profile ci --no-run \
  -E '(package(harn-vm) and binary(harn_vm)) or (package(harn-hostlib) and binary(harn_hostlib))'

# Workspace-crate cache canary touch for #5003 hosted wall-time sampling.

# Drop the linked test executables before the post step saves this target.
# Every consumer relinks them anyway: a pull request that touches harn-vm or
# anything below it (nearly all of them) rebuilds all 22 dependent workspace
# crates and relinks every test binary, so the copies only cost space in the
# 10 GB repository cache, where this family was the largest entry and its
# size pushed other merge-gate families out (#9430). Compiled libraries and
# build-script outputs stay, which is what a restore actually reuses.
target_dir="$(cargo metadata --format-version 1 --no-deps | jq -er '.target_directory')"
before_kib="$(du -sk "$target_dir" | cut -f1)"
find "$target_dir/debug/deps" -maxdepth 1 -type f -perm -u+x ! -name '*.*' -delete
after_kib="$(du -sk "$target_dir" | cut -f1)"
echo "workspace cache: dropped linked test executables, $((before_kib / 1024)) MiB -> $((after_kib / 1024)) MiB"
