#!/usr/bin/env bash
# Publication from an existing release tag must reach the publish step whether
# the tag is lightweight or annotated, and must refuse a tag that selects a
# different commit from the checkout. The tag verifier is the only reader of
# the commit a tag selects; finalization hands it the checkout to compare.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail() {
  echo "release_ship_tag_selector_test: $*" >&2
  exit 1
}

tools="$tmp/release-tools"
"$repo_root/scripts/stage_release_tools.sh" "$tools"

# Harn answers release metadata from the checkout; the gate script records the
# publish it was asked for; make stands in for the portal build.
mkdir -p "$tmp/bin"
cat > "$tmp/bin/harn" <<'EOF'
#!/usr/bin/env bash
for arg in "$@"; do
  if [[ "$arg" == current ]]; then
    awk -F'"' '/^version = "/ {print $2; exit}' Cargo.toml
    exit 0
  fi
done
exit 0
EOF
cat > "$tmp/bin/release_gate.sh" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$tmp/gate.log"
EOF
cat > "$tmp/bin/make" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$tmp/bin/harn" "$tmp/bin/release_gate.sh" "$tmp/bin/make"

git init -q --bare -b main "$tmp/origin.git"
work="$tmp/work"
git init -q -b main "$work"
git -C "$work" config user.email test@example.com
git -C "$work" config user.name test
git -C "$work" config commit.gpgSign false
git -C "$work" config tag.gpgSign false
git -C "$work" remote add origin "$tmp/origin.git"
set_version() {
  printf '[workspace.package]\nversion = "%s"\n' "$1" > "$work/Cargo.toml"
  git -C "$work" add Cargo.toml
  git -C "$work" commit -q -m "$2"
}
set_version 1.2.2 bootstrap
set_version 1.2.3 'Release v1.2.3 (#1)'
release_commit="$(git -C "$work" rev-parse HEAD)"
set_version 1.2.2 'Revert the release'
set_version 1.2.3 'Release v1.2.3 (#2)'
other_release_commit="$(git -C "$work" rev-parse HEAD)"
git -C "$work" push -q origin main

# Publish from a detached checkout of the release commit carrying a local tag,
# with origin's tag set by the caller.
finalize() {
  : > "$tmp/gate.log"
  git -C "$work" checkout -q --detach "$release_commit"
  git -C "$work" tag -f v1.2.3 "$release_commit" >/dev/null
  (cd "$work" && PATH="$tmp/bin:$PATH" HARN_BIN="$tmp/bin/harn" \
    HARN_RELEASE_ROOT="$work" HARN_RELEASE_GATE_SCRIPT="$tmp/bin/release_gate.sh" \
    "$tools/release_ship.sh" --finalize --skip-dry-run --skip-github-release) > "$tmp/out" 2>&1
}

git -C "$tmp/origin.git" update-ref refs/tags/v1.2.3 "$release_commit"
finalize || fail "a lightweight tag on the release commit was refused: $(tail -3 "$tmp/out")"
grep -qx 'publish' "$tmp/gate.log" || fail "a lightweight tag did not reach publish: $(tail -3 "$tmp/out")"

git -C "$work" tag -f -a v1.2.3 -m 'Release v1.2.3' "$release_commit" >/dev/null
git -C "$work" push -q --force origin refs/tags/v1.2.3
[[ "$(git -C "$tmp/origin.git" cat-file -t v1.2.3)" == tag ]] || fail "fixture did not publish an annotated tag"
finalize || fail "an annotated tag on the release commit was refused: $(tail -3 "$tmp/out")"
grep -qx 'publish' "$tmp/gate.log" || fail "an annotated tag did not reach publish: $(tail -3 "$tmp/out")"

# Origin's tag selects another genuine release commit on main; the checkout is
# not what the tag publishes, so nothing may be published.
git -C "$tmp/origin.git" update-ref refs/tags/v1.2.3 "$other_release_commit"
if finalize; then
  fail "a tag selecting a different commit than the checkout was published"
fi
grep -Fq "selects $other_release_commit, not the expected commit $release_commit" "$tmp/out" \
  || fail "the mismatch refusal did not name both commits: $(tail -3 "$tmp/out")"
[[ ! -s "$tmp/gate.log" ]] || fail "publish ran for a mismatched tag: $(cat "$tmp/gate.log")"

echo "release_ship_tag_selector_test: ok"
