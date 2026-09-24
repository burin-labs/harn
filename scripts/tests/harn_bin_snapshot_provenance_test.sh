#!/usr/bin/env bash
# A snapshot of a freshness-proven Harn binary stays provable, and only while
# the copy's bytes and the checkout it was proven against are unchanged. The
# release gate and `make all` hand their freshness-gated targets such a copy;
# before snapshot provenance existed every one of them refused it.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source=scripts/lib/harn_bin.sh
source "$repo_root/scripts/lib/harn_bin.sh"

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

identity=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
worktree=fedcba9876543210fedcba9876543210fedcba98
export FAKE_WORKTREE="$worktree"
export FAKE_SOURCE_PROOF=ok

# The receipt checker and the checkout fingerprint are exercised by their own
# tests; here they are the two facts the snapshot proof is built from.
harn_require_binary_freshness_receipt() {
  [[ "$FAKE_SOURCE_PROOF" = ok ]] || {
    echo "error: fake source receipt does not verify" >&2
    return 1
  }
}
harn_worktree_content_fingerprint() {
  printf '%s\n' "$FAKE_WORKTREE"
}

source_bin="$tmp_root/target/debug/harn"
mkdir -p "$tmp_root/target/debug"
printf '#!/bin/sh\necho harn\n' >"$source_bin"
chmod +x "$source_bin"
printf 'harn-bin-freshness-v6\nworktree=%s\nbuild-freshness=%s\n' \
  "$worktree" "$identity" >"$(harn_binary_freshness_receipt_path "$source_bin")"

fail() {
  echo "harn_bin_snapshot_provenance_test: $*" >&2
  exit 1
}

# A certified snapshot answers with its source's build identity.
snapshot="$(harn_snapshot_binary "$source_bin" "$tmp_root/stable" harn certify)"
[[ -r "$(harn_binary_snapshot_provenance_path "$snapshot")" ]] || fail "certify wrote no provenance"
[[ "$(harn_verified_build_freshness_id "$snapshot")" = "$identity" ]] ||
  fail "a certified snapshot did not report its source's build identity"

# The checkout moved after the build: refuse.
if FAKE_WORKTREE=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  harn_verified_build_freshness_id "$snapshot" 2>"$tmp_root/moved.err"; then
  fail "a snapshot was accepted after the checkout changed"
fi
grep -Fq "checkout changed" "$tmp_root/moved.err" || fail "the checkout refusal did not say so"

# The copy's bytes changed after certification: refuse.
printf '# tampered\n' >>"$snapshot"
if harn_verified_build_freshness_id "$snapshot" 2>"$tmp_root/bytes.err"; then
  fail "a snapshot was accepted after its bytes changed"
fi
grep -Fq "changed after it was certified" "$tmp_root/bytes.err" || fail "the byte refusal did not say so"

# A plain snapshot carries no proof, and a re-snapshot drops a stale one.
plain="$(harn_snapshot_binary "$source_bin" "$tmp_root/stable" harn)"
[[ ! -e "$(harn_binary_snapshot_provenance_path "$plain")" ]] || fail "a plain snapshot kept stale provenance"
if harn_verified_build_freshness_id "$plain" 2>/dev/null; then
  fail "an uncertified snapshot was accepted"
fi

# A source whose own receipt does not verify cannot certify a copy.
if FAKE_SOURCE_PROOF=stale harn_snapshot_binary "$source_bin" "$tmp_root/stale" harn certify \
  >/dev/null 2>&1; then
  fail "a stale source certified its snapshot"
fi

echo "harn_bin_snapshot_provenance_test: ok"
