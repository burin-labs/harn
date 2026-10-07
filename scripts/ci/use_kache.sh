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
# is shared by every runner on the host and capped. Configuration is per job:
# no `kache init`, no host config, no scheduler, no target cleanup, no remote.
#
# Usage: scripts/ci/use_kache.sh [github-env]
set -euo pipefail

KACHE_VERSION=v1.0.0
KACHE_ASSET="kache-x86_64-unknown-linux-musl.tar.gz"
KACHE_SHA256=756e9701a6afb8354fd8b1d76164e13272d320354d84f01197e57d8a3b4be397
# The binary inside that archive. Every run checks the installed copy against
# it before exporting it, so a damaged or replaced file is reinstalled rather
# than trusted for being present.
KACHE_BIN_SHA256=b826e99983057974663d6863eafbee4e1b25ca9373071406b15dd02961b2c37d

github_env=${1:-${GITHUB_ENV:?GITHUB_ENV is required}}
if [[ "$(uname -s)-$(uname -m)" != Linux-x86_64 ]]; then
  echo "::error::Kache is pinned for Linux x86_64 only, not $(uname -s)-$(uname -m)"
  exit 1
fi
max_gib=${HARN_KACHE_MAX_GIB:-100}
[[ "$max_gib" =~ ^[1-9][0-9]*$ ]] || { echo "::error::HARN_KACHE_MAX_GIB must be a positive integer"; exit 1; }

root="${HARN_KACHE_ROOT:-${XDG_CACHE_HOME:-${HOME:?}/.cache}/harn-ci-kache}"
# A host short on disk compiles without a wrapper. One owned host is a shared
# machine in the owned pool with about 114-156 GiB free, and a store allowed to
# reach its cap there would fill the disk. The existing store is left for the
# runners still using it; its size cap bounds it.
floor_gib="${HARN_KACHE_MIN_FREE_GIB:-200}"
mkdir -p "$root"
free_kib="$(df -Pk "$root" | awk 'NR == 2 { print $4 }')"
if [[ -z "$free_kib" || "$free_kib" -lt $((floor_gib * 1024 * 1024)) ]]; then
  echo "::warning::only ${free_kib:-unknown} KiB free under $root, below ${floor_gib} GiB; this job compiles without Kache"
  echo "RUSTC_WRAPPER=" >> "$github_env"
  exit 0
fi
kache="$root/bin/$KACHE_VERSION/kache"
binary_is_pinned() {
  [[ -x "$1" ]] && echo "$KACHE_BIN_SHA256  $1" | sha256sum --check --status
}
if [[ -e "$kache" ]] && ! binary_is_pinned "$kache"; then
  echo "::warning::the installed Kache $KACHE_VERSION does not match its pinned digest; reinstalling it"
  rm -f -- "$kache"
fi
if [[ ! -e "$kache" ]]; then
  mkdir -p "$root/bin/$KACHE_VERSION"
  staging="$(mktemp -d "$root/install.XXXXXX")"
  trap 'rm -rf -- "$staging"' EXIT
  # A failed download or unpack leaves the job on no compiler wrapper rather
  # than failing a required leg over a transport blip. A digest mismatch is
  # different: the release changed under its pin, and that stays fatal.
  if ! curl -fsSL --retry 3 -o "$staging/$KACHE_ASSET" \
    "https://github.com/kunobi-ninja/kache/releases/download/$KACHE_VERSION/$KACHE_ASSET"; then
    echo "::warning::Kache $KACHE_VERSION download failed; this job compiles without a wrapper"
    echo "RUSTC_WRAPPER=" >> "$github_env"
    exit 0
  fi
  echo "$KACHE_SHA256  $staging/$KACHE_ASSET" | sha256sum --check --status \
    || { echo "::error::Kache $KACHE_VERSION archive does not match its pinned digest"; exit 1; }
  unpacked=""
  if tar -xzf "$staging/$KACHE_ASSET" -C "$staging"; then
    unpacked="$(find "$staging" -type f -name kache -perm -u+x | head -1)"
  fi
  if [[ -z "$unpacked" ]]; then
    echo "::warning::Kache $KACHE_VERSION archive did not unpack to a binary; this job compiles without a wrapper"
    echo "RUSTC_WRAPPER=" >> "$github_env"
    exit 0
  fi
  binary_is_pinned "$unpacked" \
    || { echo "::error::the Kache $KACHE_VERSION binary does not match its pinned digest"; exit 1; }
  # A rename is atomic, so a concurrent job on another runner sees either no
  # binary or a complete one.
  install -m 0755 "$unpacked" "$kache.$$"
  mv -f "$kache.$$" "$kache"
fi
version="$("$kache" --version)"

mkdir -p "$root/store" "$root/run"
[[ -e "$root/kache.toml" ]] || : > "$root/kache.toml"
{
  echo "RUSTC_WRAPPER=$kache"
  echo "KACHE_CACHE_DIR=$root/store"
  echo "KACHE_RUNTIME_DIR=$root/run"
  echo "KACHE_CONFIG=$root/kache.toml"
  echo "KACHE_HOST_CONFIG="
  echo "KACHE_LOCAL_ONLY=1"
  echo "KACHE_SCHEDULER=0"
  echo "KACHE_MAX_SIZE=${max_gib}GiB"
  echo "KACHE_AUTO_CLEAN_ORPHANED_TARGETS=0"
  echo "KACHE_AUTO_CLEAN_UNUSED_UNITS_DAYS=0"
  echo "KACHE_AUTO_RECOVER_MIN_FREE_BYTES=0"
  echo "KACHE_AUTO_SHARE_TARGET_FILES=0"
} >> "$github_env"
echo "::notice::HARN_COMPILER_CACHE=kache version=\"$version\" store=$root/store max=${max_gib}GiB"
