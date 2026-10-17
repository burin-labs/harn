#!/usr/bin/env bash
# Point this job's Rust compiles at Kache on an owned runner.
#
# Kache keys a compile by its normalized inputs, so a crate one runner built is
# a hit on every other runner of the host even though each checks out under its
# own `_work/<runner>` path, and a hit is hardlinked into the target instead of
# copied. The host sccache misses there: on hetzner-1 a fresh checkout of the
# same commit rebuilt in 372-436 s with sccache against 118-147 s with Kache
# (#9435). It ties sccache on cold builds and small edits.
#
# The binary is pinned to a release of its primary source and checked against
# its digest before first use; each host keeps one copy per version. The store
# is shared by every runner on the host and capped. Runtime and configuration
# are private to this job, so a reused checkout cannot inherit another job's
# telemetry session. Configuration is per job:
# no `kache init`, no host config, no scheduler, no target cleanup, no remote.
#
# Usage: scripts/ci/use_kache.sh [github-env]
set -euo pipefail

KACHE_VERSION=v1.2.2
KACHE_ASSET="kache-x86_64-unknown-linux-musl.tar.gz"
KACHE_SHA256=f2f6555dee50945c2a90a7fb793c2084a00cc6459d08ac2306d38d905064321b

github_env=${1:-${GITHUB_ENV:?GITHUB_ENV is required}}
compile_policy=${HARN_EXT_KACHE_COMPILE_POLICY:-artifacts}
case "$compile_policy" in
  artifacts|native-incremental) ;;
  *) echo "::error::unknown Kache compile policy: $compile_policy"; exit 1 ;;
esac
if [[ "$(uname -s)-$(uname -m)" != Linux-x86_64 ]]; then
  echo "::error::Kache is pinned for Linux x86_64 only, not $(uname -s)-$(uname -m)"
  exit 1
fi
max_gib=${HARN_EXT_KACHE_MAX_GIB:-100}
[[ "$max_gib" =~ ^[1-9][0-9]*$ ]] || { echo "::error::HARN_EXT_KACHE_MAX_GIB must be a positive integer"; exit 1; }

root="${HARN_EXT_KACHE_ROOT:-${XDG_CACHE_HOME:-${HOME:?}/.cache}/harn-ci-kache}"
kache="$root/bin/$KACHE_VERSION/kache"
if [[ ! -x "$kache" ]]; then
  mkdir -p "$root/bin/$KACHE_VERSION"
  staging="$(mktemp -d "$root/install.XXXXXX")"
  trap 'rm -rf -- "$staging"' EXIT
  curl -fsSL --retry 3 -o "$staging/$KACHE_ASSET" \
    "https://github.com/kunobi-ninja/kache/releases/download/$KACHE_VERSION/$KACHE_ASSET"
  echo "$KACHE_SHA256  $staging/$KACHE_ASSET" | sha256sum --check --status \
    || { echo "::error::Kache $KACHE_VERSION archive does not match its pinned digest"; exit 1; }
  tar -xzf "$staging/$KACHE_ASSET" -C "$staging"
  unpacked="$(find "$staging" -type f -name kache -perm -u+x | head -1)"
  [[ -n "$unpacked" ]] || { echo "::error::Kache $KACHE_VERSION archive holds no kache binary"; exit 1; }
  # A rename is atomic, so a concurrent job on another runner sees either no
  # binary or a complete one.
  install -m 0755 "$unpacked" "$kache.$$"
  mv -f "$kache.$$" "$kache"
fi
version="$("$kache" --version)"

mkdir -p "$root/store"
runtime="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/harn-kache.XXXXXX")"
: > "$runtime/kache.toml"
{
  echo "RUSTC_WRAPPER=$kache"
  echo "KACHE_CACHE_DIR=$root/store"
  echo "KACHE_RUNTIME_DIR=$runtime"
  echo "KACHE_CONFIG=$runtime/kache.toml"
  # Persistent targets live outside the checkout. Pin telemetry to this job's
  # source root instead of the common ancestor Kache infers from output paths.
  echo "KACHE_EVENT_ROOT=${GITHUB_WORKSPACE:-$PWD}"
  echo "KACHE_HOST_CONFIG="
  echo "KACHE_LOCAL_ONLY=1"
  echo "KACHE_SCHEDULER=0"
  echo "KACHE_MAX_SIZE=${max_gib}GiB"
  echo "KACHE_AUTO_CLEAN_ORPHANED_TARGETS=0"
  echo "KACHE_AUTO_CLEAN_UNUSED_UNITS_DAYS=0"
  echo "KACHE_AUTO_RECOVER_MIN_FREE_BYTES=0"
  echo "KACHE_AUTO_RECOVER_TARGETS=0"
  echo "KACHE_AUTO_SHARE_TARGET_FILES=0"
  if [[ "$compile_policy" == native-incremental ]]; then
    # Only the native library keeps incremental state in this runner's private
    # target. Other crates still use shared compiler objects. These settings
    # were measured together through canonical CLI packaging and restoration.
    echo "CARGO_INCREMENTAL=1"
    echo "CARGO_PROFILE_CI_CLI_CODEGEN_UNITS=16"
    echo "KACHE_ADAPTIVE_INCREMENTAL=0"
    echo "KACHE_INCREMENTAL_CRATES=harn_vm"
  fi
} >> "$github_env"
echo "::notice::HARN_COMPILER_CACHE=kache version=\"$version\" policy=$compile_policy store=$root/store max=${max_gib}GiB"
