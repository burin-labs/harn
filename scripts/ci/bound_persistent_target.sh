#!/usr/bin/env bash
# Keep a self-hosted runner's persistent Cargo target from filling its disk.
#
# The macOS workspace job reuses one target per runner across runs and nothing
# else prunes it, so `debug/deps` collects every superseded dependency
# generation. Before a build, empty the target when it is over a size ceiling or
# when the volume holding it is short on free space. Emptying costs one cold
# build; a full disk fails release qualification.
#
# Usage: bound_persistent_target.sh <target-dir>
# Environment: HARN_PERSISTENT_TARGET_MAX_GIB (default 80),
# HARN_PERSISTENT_TARGET_MIN_FREE_GIB (default 60). The target must be an
# existing directory under $HOME/harn-ci/target.
set -euo pipefail

target=${1:?usage: bound_persistent_target.sh <target-dir>}
max_gib=${HARN_PERSISTENT_TARGET_MAX_GIB:-80}
min_free_gib=${HARN_PERSISTENT_TARGET_MIN_FREE_GIB:-60}
root=${HARN_PERSISTENT_TARGET_ROOT:-$HOME/harn-ci/target}

[[ "$max_gib" =~ ^[0-9]+$ && "$min_free_gib" =~ ^[0-9]+$ ]] || { echo "limits must be whole GiB" >&2; exit 2; }
[[ -d "$target" ]] || { echo "not a directory: $target" >&2; exit 2; }
target=$(cd "$target" && pwd -P)
root=$(cd "$root" && pwd -P)
[[ "$target" == "$root"/?* && "$target" != *..* ]] || { echo "refusing a target outside $root: $target" >&2; exit 2; }

used_kib=$(du -sk "$target" | awk '{print $1}')
free_kib=$(df -k "$target" | awk 'NR == 2 {print $4}')
[[ "$used_kib" =~ ^[0-9]+$ && "$free_kib" =~ ^[0-9]+$ ]] || { echo "could not measure $target" >&2; exit 1; }

echo "persistent target $target: used $((used_kib / 1048576)) GiB (ceiling $max_gib), volume free $((free_kib / 1048576)) GiB (floor $min_free_gib)"
if ((used_kib > max_gib * 1048576)); then
  reason="over the $max_gib GiB ceiling"
elif ((free_kib < min_free_gib * 1048576)); then
  reason="volume below $min_free_gib GiB free"
else
  echo "within bounds; keeping it"
  exit 0
fi
echo "emptying the target: $reason"
find "$target" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
