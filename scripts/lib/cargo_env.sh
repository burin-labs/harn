#!/bin/sh

# Shared Cargo environment helpers for Harn's local hooks and CI/release
# warm-build scripts. Cargo normally keeps intermediates in the target
# directory, which is already stable and isolated per worktree. When a caller
# explicitly selects another target, align any inherited custom build-dir with
# that boundary as well.

harn_cargo_metadata_target_dir() (
  restricted_decoder=0
  metadata=$(cargo metadata --format-version=1 --no-deps) || exit $?
  if command -v jq >/dev/null 2>&1; then
    # Cargo owns this path and exposes it through a JSON contract. Delegate
    # JSON decoding to jq so escaped native Windows separators, drive colons,
    # spaces, and Unicode are data rather than shell/regex syntax. The cache
    # below is line-oriented, so fail closed on paths it cannot represent.
    target_dir=$(printf '%s\n' "$metadata" | jq -er '
      .target_directory
      | strings
      | select(length > 0)
      | select((contains("\n") or contains("\r")) | not)
    ') || target_dir=
  else
    restricted_decoder=1
    # Keep Rust-only Unix checkouts usable without adding jq as a build
    # prerequisite. This fallback intentionally accepts only Cargo JSON whose
    # target path needs no JSON escapes; anything broader requires a real JSON
    # decoder instead of hand-rolled unescaping.
    target_dir=$(printf '%s\n' "$metadata" \
      | sed -n 's/.*"target_directory":"\([^"\\]*\)".*/\1/p')
  fi
  if [ -z "$target_dir" ]; then
    if [ "$restricted_decoder" = 1 ]; then
      echo "error: jq is required when Cargo's target_directory contains JSON escapes" >&2
    fi
    echo "error: cargo metadata did not report a supported nonempty target_directory" >&2
    exit 1
  fi

  case "${OS:-$(uname -s)}" in
    Windows_NT | MINGW* | MSYS* | CYGWIN*)
      if ! command -v cygpath >/dev/null 2>&1; then
        echo "error: cannot normalize Cargo's native Windows target_directory without cygpath" >&2
        exit 1
      fi
      target_dir=$(cygpath -u "$target_dir") || exit $?
      ;;
  esac
  printf '%s\n' "$target_dir"
)

# Resolve the lease policy once for every Cargo entry point. CI intentionally
# runs without a local cross-process lease unless a caller explicitly supplies
# one; developer shells default to auto-discovery, while an explicit runner is
# a required authority. Keeping this policy here prevents wrappers and their
# callers from disagreeing about whether control environment must traverse a
# lease supervisor.
harn_effective_cargo_lease_mode() {
  lease_mode=${HARN_CARGO_LEASE_MODE:-}
  if [ -z "$lease_mode" ]; then
    if [ -n "${HARN_CARGO_LEASE_RUNNER:-}" ]; then
      lease_mode=required
    elif [ "${CI:-}" = "true" ]; then
      lease_mode=off
    else
      lease_mode=auto
    fi
  fi

  case "$lease_mode" in
    auto | off | required) printf '%s\n' "$lease_mode" ;;
    *)
      echo "error: HARN_CARGO_LEASE_MODE must be auto, off, or required" >&2
      return 2
      ;;
  esac
}

harn_export_cargo_build_dir_for_target() {
  target_dir=${1:-${CARGO_TARGET_DIR:-}}
  if [ -z "$target_dir" ]; then
    return 1
  fi
  if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then
    return 1
  fi

  # Cargo's separate build-dir path is redundant when it equals target-dir.
  # On native Windows, enabling it also makes Cargo hand verbatim `\\?\` OUT_DIR
  # paths to C/C++ build scripts. cc-rs forwards those to cl.exe, which can
  # reinterpret the source path as rooted at `\\` and fail to find it. Keep an
  # explicit caller-owned build-dir, but let Cargo's target-local default own
  # intermediates when a Windows caller only selected CARGO_TARGET_DIR.
  case "${OS:-$(uname -s)}" in
    Windows_NT | MINGW* | MSYS* | CYGWIN*) return 1 ;;
  esac

  CARGO_BUILD_BUILD_DIR=$target_dir
  export CARGO_BUILD_BUILD_DIR
  return 0
}

# The generated rustc wrapper that dev setup names in each worktree's
# `.cargo/config.toml`. It lives in shared storage, so anything that reclaims
# that storage can delete it while every configured worktree still points at
# it, and then every Cargo command fails with a bare "No such file or
# directory" from the missing wrapper. The artifact is addressed by its source
# hash and host triple, so the same function that dev setup uses to build it
# can rebuild it in place, and the Cargo entry points do exactly that.

# Digest the bytes that identify a wrapper source or private source snapshot.
harn_sccache_wrapper_source_digest() (
  digest=$(shasum -a 256 < "$1") || exit $?
  printf '%s\n' "${digest%% *}"
)

# Print the wrapper path for `root`'s wrapper source under `storage`.
harn_sccache_wrapper_path() (
  root="$1"
  storage="$2"
  source_hash=$(harn_sccache_wrapper_source_digest "$root/scripts/sccache_rustc_wrapper.rs") || exit $?
  host_triple=$(rustc -vV | sed -n 's/^host: //p')
  if [ -z "$host_triple" ]; then
    echo "error: rustc did not report a host triple for the sccache wrapper" >&2
    exit 1
  fi
  suffix=""
  case "${OS:-$(uname -s)}" in
    Windows_NT | MINGW* | MSYS* | CYGWIN*) suffix=".exe" ;;
  esac
  printf '%s\n' "$storage/sccache-wrapper/$host_triple-$source_hash/harn-sccache-wrapper$suffix"
)

# Build `root`'s wrapper under `storage` unless it already exists, then print
# its path.
harn_build_sccache_wrapper() (
  root="$1"
  storage="$2"
  wrapper_path=$(harn_sccache_wrapper_path "$root" "$storage") || exit $?
  if [ ! -x "$wrapper_path" ]; then
    mkdir -p "$(dirname "$wrapper_path")" || exit $?
    temporary_path=$(mktemp "$wrapper_path.tmp.XXXXXX") || exit $?
    source_snapshot="$temporary_path.sccache_rustc_wrapper.rs"
    trap 'rm -f "$temporary_path" "$source_snapshot"' EXIT
    source_key=${wrapper_path%/*}
    source_key=${source_key##*-}
    cp "$root/scripts/sccache_rustc_wrapper.rs" "$source_snapshot" || exit $?
    if [ "$(harn_sccache_wrapper_source_digest "$source_snapshot")" != "$source_key" ]; then
      echo "error: compiler-cache wrapper source changed before compilation" >&2
      exit 1
    fi
    rustc --edition=2021 --crate-name sccache_rustc_wrapper -C strip=symbols "$source_snapshot" \
      -o "$temporary_path" || exit $?
    if [ "$(harn_sccache_wrapper_source_digest "$root/scripts/sccache_rustc_wrapper.rs")" != "$source_key" ] \
      || [ "$(harn_sccache_wrapper_source_digest "$source_snapshot")" != "$source_key" ]; then
      echo "error: compiler-cache wrapper source changed during compilation; not publishing" >&2
      exit 1
    fi
    # A same-filesystem hard link publishes the complete executable atomically
    # and never replaces a live wrapper, including on Windows. A concurrent
    # publisher may win first; its source-addressed artifact is equivalent.
    if ! ln "$temporary_path" "$wrapper_path" 2>/dev/null \
      && [ ! -x "$wrapper_path" ]; then
      rm -f "$temporary_path"
      echo "error: failed to publish the sccache wrapper" >&2
      exit 1
    fi
  fi
  printf '%s\n' "$wrapper_path"
)

# Rebuild the wrapper `workspace`'s generated Cargo config names when it is
# missing. Only the dev-setup managed wrapper is touched, and only when its
# configured path is the one this checkout's source builds; a path from an
# older wrapper source cannot be rebuilt in place, so that case names the
# command that regenerates the config instead of letting Cargo fail obscurely.
harn_restore_configured_sccache_wrapper() {
  _harn_workspace="$1"
  _harn_config="$_harn_workspace/.cargo/config.toml"
  [ -f "$_harn_config" ] || return 0
  _harn_wrapper=$(sed -n 's/^rustc-wrapper[[:space:]]*=[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p' "$_harn_config" | head -n 1)
  case "$_harn_wrapper" in
    */sccache-wrapper/*/harn-sccache-wrapper | */sccache-wrapper/*/harn-sccache-wrapper.exe) ;;
    *) return 0 ;;
  esac
  [ -x "$_harn_wrapper" ] && return 0
  _harn_storage=$(dirname "$(dirname "$(dirname "$_harn_wrapper")")")
  _harn_expected=$(harn_sccache_wrapper_path "$_harn_workspace" "$_harn_storage") || return $?
  if [ "$_harn_expected" != "$_harn_wrapper" ]; then
    echo "error: the rustc wrapper .cargo/config.toml names is missing: $_harn_wrapper" >&2
    echo "       it was built from an older wrapper source; run \`make refresh-cargo-wrapper\` to refresh only that input" >&2
    return 1
  fi
  echo "note: rebuilding the missing rustc wrapper .cargo/config.toml names: $_harn_wrapper" >&2
  harn_build_sccache_wrapper "$_harn_workspace" "$_harn_storage" >/dev/null
}
