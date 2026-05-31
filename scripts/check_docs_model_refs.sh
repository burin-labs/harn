#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
providers_toml="$repo_root/crates/harn-vm/src/llm/providers.toml"

alias_id() {
  local alias="$1"
  awk -v section="[aliases.${alias}]" '
    $0 == section { in_section = 1; next }
    in_section && /^\[/ { exit }
    in_section && /^[[:space:]]*id[[:space:]]*=/ {
      line = $0
      sub(/^[^"]*"/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }
  ' "$providers_toml"
}

sonnet_model="$(alias_id sonnet)"
if [[ -z "$sonnet_model" ]]; then
  echo "error: could not resolve aliases.sonnet from $providers_toml" >&2
  exit 1
fi

mapfile -d '' docs_files < <(
  find "$repo_root/docs/src" "$repo_root/docs/llm" -type f -name '*.md' -print0
)

checked_files=()
for file in "${docs_files[@]}"; do
  if head -n 5 "$file" | grep -q 'GENERATED'; then
    continue
  fi
  checked_files+=("$file")
done

pattern='claude-sonnet-4(-[0-9][0-9A-Za-z]*)+'
failures=0
while IFS=: read -r file line model; do
  if [[ "$model" == "$sonnet_model" ]]; then
    continue
  fi
  if sed -n "${line}p" "$file" | grep -q 'harn-doc-model-ref: allow-stale'; then
    continue
  fi
  rel="${file#"$repo_root/"}"
  echo "error: $rel:$line references $model, but aliases.sonnet is $sonnet_model" >&2
  echo "       update the docs example or add harn-doc-model-ref: allow-stale if this is intentionally historical." >&2
  failures=$((failures + 1))
done < <(rg -n -o "$pattern" "${checked_files[@]}" || true)

if (( failures > 0 )); then
  exit 1
fi

echo "docs model refs OK: aliases.sonnet = $sonnet_model"
