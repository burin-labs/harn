#!/usr/bin/env bash
# Proves the Kache canary cannot corrupt a host-shared store through a
# persistent target (#9435). Builds a crate with a proc macro and a build
# script through scripts/ci/use_kache.sh in one checkout, restores it into a
# second, then attacks every target file that shares an inode with the store:
#   - an in-place write must be refused;
#   - a replace-by-rename, which is how Cargo and rustc update outputs, must
#     leave the store blob byte-identical;
# and finally `kache doctor --verify` must still report a clean store.
# Linux x86_64 only. CI runs it in the workspace test producer whenever that
# job lands on an owned runner.
set -euo pipefail

if [[ "$(uname -s)-$(uname -m)" != Linux-x86_64 ]]; then
  echo "kache store integrity: skipped (Linux x86_64 only)"
  exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'env_kache daemon stop >/dev/null 2>&1 || true; chmod -R u+w "$tmp" 2>/dev/null || true; rm -rf "$tmp"' EXIT
fail() { echo "kache store integrity: FAIL: $*" >&2; exit 1; }

# A host that already installed the pinned Kache (checked against its digest)
# lends that binary, so the proof needs no download; otherwise use_kache.sh
# fetches and verifies it.
if [[ -n "${KACHE_INTEGRITY_SEED_BIN:-}" && "$(basename -- "$KACHE_INTEGRITY_SEED_BIN")" == kache \
  && -x "$KACHE_INTEGRITY_SEED_BIN" ]]; then
  mkdir -p "$tmp/root/bin/v1.0.0"
  cp "$KACHE_INTEGRITY_SEED_BIN" "$tmp/root/bin/v1.0.0/kache"
fi
# The proof builds one small crate, so the host's disk floor does not apply.
HARN_KACHE_MIN_FREE_GIB=1 HARN_KACHE_ROOT="$tmp/root" \
  "$repo_root/scripts/ci/use_kache.sh" "$tmp/env" > /dev/null
set -a
# shellcheck disable=SC1091
. "$tmp/env"
set +a
[[ "$(basename -- "${RUSTC_WRAPPER:-}")" == kache ]] || fail "use-kache.sh did not configure Kache"
env_kache() { "$RUSTC_WRAPPER" "$@"; }
export CARGO_HOME="$tmp/cargo-home" CARGO_TERM_COLOR=never

cargo new -q --lib "$tmp/first"
cat >> "$tmp/first/Cargo.toml" <<'TOML'
serde = { version = "1", features = ["derive"] }
itoa = "1"
TOML
printf 'fn main() { println!("cargo:rerun-if-changed=build.rs"); }\n' > "$tmp/first/build.rs"
(cd "$tmp/first" && cargo build -q)
cp -r "$tmp/first" "$tmp/second"
rm -rf "$tmp/second/target"
(cd "$tmp/second" && cargo build -q)

store="$tmp/root/store"
mapfile -t linked < <(
  find "$tmp/second/target" -type f -links +1 -print0 | while IFS= read -r -d '' file; do
    inode="$(stat -c %i "$file")"
    if find "$store" -inum "$inode" -print -quit | grep -q .; then
      printf '%s\n' "$file"
    fi
  done
)
((${#linked[@]} > 0)) || fail "no target file shares an inode with the store; the attack proves nothing"
echo "target files hardlinked to the store: ${#linked[@]}"

for file in "${linked[@]}"; do
  inode="$(stat -c %i "$file")"
  blob="$(find "$store" -inum "$inode" -print -quit)"
  before="$(sha256sum "$blob" | cut -d' ' -f1)"
  mode="$(stat -c %A "$file")"
  [[ "$mode" != *w* ]] || fail "$file is writable ($mode) while it shares the store blob"
  # In place: must be refused.
  if (printf 'corrupt' >> "$file") 2>/dev/null; then
    fail "an in-place append to $file succeeded"
  fi
  # Replace by rename, as Cargo and rustc do: the store must not change.
  printf 'replacement' > "$file.new"
  mv -f "$file.new" "$file"
  after="$(sha256sum "$blob" | cut -d' ' -f1)"
  [[ "$before" == "$after" ]] || fail "replacing $file changed store blob $blob"
  echo "ok: $(basename -- "$file") ($mode) refuses in-place writes; replacing it left the store blob unchanged"
done

env_kache doctor --verify > "$tmp/doctor.log" 2>&1 || { cat "$tmp/doctor.log" >&2; fail "kache doctor --verify reports a damaged store"; }
echo "kache store integrity: ok (${#linked[@]} linked outputs attacked; doctor --verify clean)"
