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

# Warm only the test graph consumed by rust-check-inputs. The shared CLI has
# its own ci-cli producer and cache; a dev-profile CLI build here adds another
# dependency graph that neither consumer restores for execution.
host_bound_filter="$(scripts/ci/host_bound_rust_test_filter.sh)"
warm_phase workspace-tests cargo-nextest nextest run --locked --workspace --profile ci --no-run \
  -E "not (${host_bound_filter})"
# Match the security producer's two packages and workspace features.
security_features="$(bash scripts/ci/workspace_features.sh harn-vm harn-hostlib)"
[[ -n "$security_features" ]] || { echo 'security warm has no measured workspace features' >&2; exit 1; }
warm_phase security-tests cargo-nextest nextest run --locked -p harn-vm -p harn-hostlib \
  --features "$security_features" --profile ci --no-run \
  -E '(package(harn-vm) and binary(harn_vm)) or (package(harn-hostlib) and binary(harn_hostlib))'

# Workspace-crate cache canary touch for #5003 hosted wall-time sampling.

# Retain Native test executables: build stamps no longer invalidate Native,
# and a revision-only control reused the exact unit binary instead of paying
# another compile and link. Other linked test binaries still yield space to
# the compiled libraries. Admission reserves replacement space before upload.
target_dir="$(cargo metadata --format-version 1 --no-deps | jq -er '.target_directory')"
before_kib="$(du -sk "$target_dir" | cut -f1)"
native_count="$(find "$target_dir/debug/deps" -maxdepth 1 -type f -perm -u+x ! -name '*.*' -name 'harn_vm-*' | wc -l | tr -d ' ')"
if [[ ! "$native_count" =~ ^[1-9][0-9]*$ ]]; then
  echo 'workspace cache: Native test executables were not observed; refusing to publish' >&2
  exit 1
fi
find "$target_dir/debug/deps" -maxdepth 1 -type f -perm -u+x ! -name '*.*' ! -name 'harn_vm-*' -delete
after_kib="$(du -sk "$target_dir" | cut -f1)"
echo "workspace cache: retained_native_executables=$native_count; pruned other linked tests, $((before_kib / 1024)) MiB -> $((after_kib / 1024)) MiB"
