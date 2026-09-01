#!/usr/bin/env bash
set -euo pipefail

trim() {
  local value=$1
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  printf '%s' "$value"
}

explicit_size="$(trim "${HARN_SCCACHE_CACHE_SIZE_INPUT:-}")"
inherited_size="$(trim "${SCCACHE_CACHE_SIZE:-}")"

if [[ -n "$explicit_size" ]]; then
  effective_size=$explicit_size
elif [[ -n "$inherited_size" ]]; then
  effective_size=$inherited_size
else
  effective_size=2G
fi

# Keep the action boundary narrower than sccache's permissive parser. CI cache
# ceilings are positive integral byte quantities with an optional binary or
# decimal unit; malformed values must not silently become an unbounded cache.
if [[ ! "$effective_size" =~ ^[1-9][0-9]*([KMGT]i?B?|B)?$ ]]; then
  echo "error: invalid sccache cache size: ${effective_size}" >&2
  exit 2
fi

: "${HOME:?HOME must name the cache owner directory}"
: "${GITHUB_ENV:?GITHUB_ENV must name the Actions environment file}"
sccache_dir="${HOME}/.cache/sccache"
mkdir -p "$sccache_dir"

{
  echo "SCCACHE_GHA_ENABLED=false"
  echo "SCCACHE_DIR=${sccache_dir}"
  echo "SCCACHE_CACHE_SIZE=${effective_size}"
  # sccache's server exits after SCCACHE_IDLE_TIMEOUT seconds with no request;
  # the default is 600. A release lane can easily exceed ten minutes between
  # compiler invocations, such as during a long link or serialized codegen
  # tail. When the server dies mid-build, rustc silently falls back to compiling
  # locally. Zero keeps the server alive until the runner tears it down.
  echo "SCCACHE_IDLE_TIMEOUT=0"
} >> "$GITHUB_ENV"

echo "::notice title=sccache cache ceiling::effective persistent cache size ${effective_size}"
