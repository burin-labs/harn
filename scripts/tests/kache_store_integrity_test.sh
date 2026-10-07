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
# Each checkout builds into its own explicit target. A job may already export
# CARGO_TARGET_DIR (an owned runner's persistent target), and building there
# would leave the scanned directories empty and the proof vacuous.
(cd "$tmp/first" && cargo build -q --target-dir "$tmp/first/target")
cp -r "$tmp/first" "$tmp/second"
rm -rf "$tmp/second/target"
(cd "$tmp/second" && cargo build -q --target-dir "$tmp/second/target")

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

# A Kache job and a later non-Kache job on the same owned runner. Read-only
# Kache outputs left in a persistent target that a later non-Kache build reuses
# broke a downstream main run on the same hosts. The rust-cache action now
# uses the runner's persistent target only when the job did not end up on
# Kache; this replays that gate, job after job, against one runner directory.
action="$repo_root/.github/actions/rust-cache/action.yml"
gate_line="$(awk "/name: Use the runner's persistent target directory/ {getline; getline; print; exit}" "$action")"
[[ "$gate_line" == *"!endsWith(env.RUSTC_WRAPPER, '/kache')"* ]] \
  || fail "the persistent target step must exclude jobs compiling through Kache (got: $gate_line)"

replay_jobs() { # gate(new|old) -> "files the Kache job left in the persistent target" "unwritable files after job 2"
  local gate=$1 checkout="$tmp/checkout-$1" persistent="$tmp/runner-$1/harn-ci-target/workspace-tests/replay-w1/target"
  rm -rf "$checkout"
  cp -r "$tmp/first" "$checkout"
  rm -rf "$checkout/target"
  mkdir -p "$persistent"
  # Job 1 compiles through Kache: the old gate gave it the persistent target.
  local kache_target="$checkout/target"
  [[ $gate == old ]] && kache_target="$persistent"
  (cd "$checkout" && cargo build -q --target-dir "$kache_target") || fail "the Kache job ($gate gate) did not build"
  local leaked
  leaked="$(find "$persistent" -type f | wc -l | tr -d ' ')"
  # Job 2, without Kache, after checkout's clean and a source change.
  rm -rf "$checkout/target"
  printf '\npub fn replay() {}\n' >> "$checkout/src/lib.rs"
  (cd "$checkout" && RUSTC_WRAPPER='' cargo build -q --target-dir "$persistent") \
    || fail "the non-Kache job after a Kache job failed ($gate gate)"
  echo "$leaked $(find "$persistent" -type f ! -perm -u+w | wc -l | tr -d ' ')"
}
read -r new_leaked new_unwritable < <(replay_jobs new)
read -r old_leaked _ < <(replay_jobs old)
[[ "$new_leaked" == 0 ]] || fail "a Kache job wrote $new_leaked files into the persistent target"
[[ "$new_unwritable" == 0 ]] || fail "the persistent target holds $new_unwritable unwritable files after a Kache job"
# Negative control: the old gate let the same Kache job write into the target
# the next job reuses, so this replay can see the leak.
((old_leaked > 0)) || fail "the replay of the old gate saw no leak; the check above proves nothing"
echo "ok: a non-Kache build after a Kache build on one runner succeeds; the Kache job left 0 files in the persistent target (the old gate left $old_leaked)"

env_kache doctor --verify > "$tmp/doctor.log" 2>&1 || { cat "$tmp/doctor.log" >&2; fail "kache doctor --verify reports a damaged store"; }
echo "kache store integrity: ok (${#linked[@]} linked outputs attacked; doctor --verify clean)"
