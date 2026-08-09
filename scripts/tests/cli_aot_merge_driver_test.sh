#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

work="$tmp_root/work"
git init --initial-branch main --quiet "$work"
git -C "$work" config user.email "test@example.com"
git -C "$work" config user.name "Test User"
git -C "$work" config commit.gpgsign false

mkdir -p "$work/crates/harn-stdlib/src/stdlib/cli/providers"
mkdir -p "$work/crates/harn-stdlib/src/stdlib/cli/models"
cp "$repo_root/.gitattributes" "$work/.gitattributes"
cp "$repo_root/.gitignore" "$work/.gitignore"

printf '%s\n' 'base provider source' \
  > "$work/crates/harn-stdlib/src/stdlib/cli/providers/tool_scorecard.harn"
printf '%s\n' 'base model source' \
  > "$work/crates/harn-stdlib/src/stdlib/cli/models/list.harn"
git -C "$work" add .
git -C "$work" commit --quiet -m "base authoring sources"

write_ignored_payload() {
  local label="$1"
  mkdir -p "$work/crates/harn-cli/generated/cli-bytecode"
  printf '%s\n' "$label manifest" > "$work/crates/harn-cli/generated/cli-bytecode-manifest.json"
  printf '\0%s-provider' "$label" \
    > "$work/crates/harn-cli/generated/cli-bytecode/providers-tool_scorecard.harnbc"
}

git -C "$work" checkout --quiet -b feature
printf '%s\n' 'feature provider source' \
  > "$work/crates/harn-stdlib/src/stdlib/cli/providers/tool_scorecard.harn"
write_ignored_payload feature
git -C "$work" add .
git -C "$work" commit --quiet -m "feature authoring source"

git -C "$work" checkout --quiet main
printf '%s\n' 'main model source' \
  > "$work/crates/harn-stdlib/src/stdlib/cli/models/list.harn"
write_ignored_payload main
git -C "$work" add .
git -C "$work" commit --quiet -m "main authoring source"

git -C "$work" merge --quiet --no-edit feature

if [[ "$(<"$work/crates/harn-stdlib/src/stdlib/cli/providers/tool_scorecard.harn")" != "feature provider source" ]]; then
  echo "feature authoring source was not merged" >&2
  exit 1
fi
if [[ "$(<"$work/crates/harn-stdlib/src/stdlib/cli/models/list.harn")" != "main model source" ]]; then
  echo "current authoring source changed unexpectedly" >&2
  exit 1
fi
if [[ -n "$(git -C "$work" ls-files -u)" ]]; then
  echo "authoring merge left unresolved paths" >&2
  exit 1
fi

for path in \
  crates/harn-cli/generated/cli-bytecode-manifest.json \
  crates/harn-cli/generated/cli-bytecode/providers-tool_scorecard.harnbc; do
  if ! git -C "$work" check-ignore -q -- "$path"; then
    echo "release/package CLI AOT payload is not ignored: $path" >&2
    exit 1
  fi
  if git -C "$work" ls-files --error-unmatch -- "$path" >/dev/null 2>&1; then
    echo "release/package CLI AOT payload is still tracked: $path" >&2
    exit 1
  fi
  if ! git -C "$work" check-attr merge -- "$path" | grep -Fxq "$path: merge: unspecified"; then
    echo "release/package CLI AOT payload still has custom merge handling: $path" >&2
    exit 1
  fi
done

echo "cli_aot_merge_driver_test: ok"
