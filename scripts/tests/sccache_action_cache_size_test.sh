#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
configure="$repo_root/.github/actions/sccache/configure-env.sh"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-sccache-action.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT

assert_assignment() {
  local env_file=$1
  local name=$2
  local expected=$3
  local actual
  actual="$(sed -n "s/^${name}=//p" "$env_file")"
  if [[ "$actual" != "$expected" ]]; then
    echo "error: expected ${name}=${expected}, got ${actual:-<missing>}" >&2
    exit 1
  fi
  if [[ "$(grep -c "^${name}=" "$env_file")" != "1" ]]; then
    echo "error: expected exactly one ${name} assignment" >&2
    exit 1
  fi
}

assert_configures() {
  local case_name=$1
  local expected=$2
  local explicit=$3
  local inherited=$4
  local case_root="$test_root/$case_name"
  local env_file="$case_root/github-env"
  mkdir -p "$case_root"
  : > "$env_file"

  HOME="$case_root/home" \
    GITHUB_ENV="$env_file" \
    HARN_SCCACHE_CACHE_SIZE_INPUT="$explicit" \
    SCCACHE_CACHE_SIZE="$inherited" \
    "$configure" >/dev/null

  assert_assignment "$env_file" SCCACHE_GHA_ENABLED false
  assert_assignment "$env_file" SCCACHE_DIR "$case_root/home/.cache/sccache"
  assert_assignment "$env_file" SCCACHE_CACHE_SIZE "$expected"
  assert_assignment "$env_file" SCCACHE_IDLE_TIMEOUT 0
  test -d "$case_root/home/.cache/sccache"
}

assert_rejected() {
  local case_name=$1
  local explicit=$2
  local inherited=$3
  local case_root="$test_root/$case_name"
  local env_file="$case_root/github-env"
  mkdir -p "$case_root"
  : > "$env_file"

  if HOME="$case_root/home" \
    GITHUB_ENV="$env_file" \
    HARN_SCCACHE_CACHE_SIZE_INPUT="$explicit" \
    SCCACHE_CACHE_SIZE="$inherited" \
    "$configure" >/dev/null 2>&1; then
    echo "error: invalid cache-size contract was accepted: ${explicit}/${inherited}" >&2
    exit 1
  fi
  if [[ -s "$env_file" ]]; then
    echo "error: rejected cache-size contract wrote a partial environment" >&2
    exit 1
  fi
}

# An explicit action input is the narrow override. Otherwise the workflow's
# declared ceiling survives; only a fully absent contract gets the bounded
# action default.
assert_configures explicit-wins 2G 2G 15G
assert_configures inherited-wins 15G "" 15G
assert_configures default 2G "" ""
assert_configures normalized 15GiB " 15GiB " 2G
assert_configures ignored-invalid-inherited 2G 2G unlimited

assert_rejected zero-explicit 0G 15G
assert_rejected text-explicit unlimited 15G
assert_rejected spaces-explicit '15 G' 15G
assert_rejected zero-inherited "" 0G
assert_rejected text-inherited "" unlimited
assert_rejected newline-injection "" $'15G\nOTHER=value'

echo "sccache action cache-size tests passed"
