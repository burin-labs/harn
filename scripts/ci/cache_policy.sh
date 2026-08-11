# Shared loader for .github/cache-policy.json.
# Sourced by CI scripts; do not execute directly.
#
# Override the document with HARN_CACHE_POLICY_PATH for tests. Knobs stay in
# that JSON; callers project fields into shell variables.

_HARN_CACHE_POLICY_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
_HARN_CACHE_POLICY_REPO_ROOT="$(cd "${_HARN_CACHE_POLICY_SCRIPT_DIR}/../.." && pwd -P)"
readonly HARN_CACHE_POLICY_SCHEMA_VERSION=4

harn_cache_policy_path() {
  printf '%s\n' "${HARN_CACHE_POLICY_PATH:-$_HARN_CACHE_POLICY_REPO_ROOT/.github/cache-policy.json}"
}

harn_cache_policy_require() {
  local path schema
  path="$(harn_cache_policy_path)"
  if [[ ! -f "$path" ]]; then
    echo "error: cache policy not found: $path" >&2
    exit 2
  fi
  schema="$(jq -er '.schema_version' "$path")"
  if [[ "$schema" != "$HARN_CACHE_POLICY_SCHEMA_VERSION" ]]; then
    echo "error: expected cache-policy.json schema_version ${HARN_CACHE_POLICY_SCHEMA_VERSION}, got ${schema}" >&2
    exit 2
  fi
}

# jq -er <filter> against the active cache policy document.
harn_cache_policy_jq() {
  local filter=$1
  harn_cache_policy_require
  jq -er "$filter" "$(harn_cache_policy_path)"
}
