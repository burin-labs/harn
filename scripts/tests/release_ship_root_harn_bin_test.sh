#!/usr/bin/env bash
# Publication stages release tools from main and builds Harn from the release
# root. The Harn binary must be resolved by the release root's own harn_bin.sh,
# whose freshness proof matches the tree it builds; main's staged copy is only
# the fallback for a root that has none.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail() {
  echo "release_ship_root_harn_bin_test: $*" >&2
  exit 1
}

tools="$tmp/release-tools"
"$repo_root/scripts/stage_release_tools.sh" "$tools"
record="$tmp/record"

# Both resolvers record who was asked and refuse, so the run stops at the first
# Harn resolution and the record names its owner.
write_stub() {
  local path="$1" owner="$2"
  mkdir -p "$(dirname "$path")"
  cat > "$path" <<EOF
#!/usr/bin/env bash
printf '%s %s\n' "$owner" "\$*" >> "$record"
exit 3
EOF
  chmod +x "$path"
}
write_stub "$tools/harn_bin.sh" staged

root="$tmp/release-root"
git init -q -b main "$root"
git -C "$root" config user.email test@example.com
git -C "$root" config user.name test
git -C "$root" config commit.gpgSign false
printf '[workspace.package]\nversion = "1.2.3"\n' > "$root/Cargo.toml"
write_stub "$root/scripts/harn_bin.sh" root
git -C "$root" add -A
git -C "$root" commit -q -m 'Release v1.2.3 (#1)'

run_finalize() {
  : > "$record"
  (cd "$root" && env -u HARN_BIN -u HARN_RELEASE_METADATA_BIN \
    HARN_RELEASE_ROOT="$root" \
    "$tools/release_ship.sh" --finalize --skip-github-release) > "$tmp/out" 2>&1 || true
}

run_finalize
first="$(head -1 "$record" || true)"
[[ "$first" == root\ * ]] \
  || fail "the first Harn resolution was not the release root's harn_bin.sh: '${first:-<none>}' ($(cat "$tmp/out"))"
if grep -q '^staged ' "$record"; then
  fail "main's staged harn_bin.sh resolved Harn for a release root that has its own: $(cat "$record")"
fi

# A root without harn_bin.sh falls back to the staged copy.
git -C "$root" rm -q scripts/harn_bin.sh
git -C "$root" commit -q -m 'Release v1.2.3 (#1)' --amend
run_finalize
first="$(head -1 "$record" || true)"
[[ "$first" == staged\ * ]] \
  || fail "a root without harn_bin.sh did not fall back to the staged copy: '${first:-<none>}'"

echo "release_ship_root_harn_bin_test: ok"
