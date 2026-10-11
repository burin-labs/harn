#!/usr/bin/env bash
# A reclaim pass deleted the shared rustc wrapper every worktree's generated
# Cargo config names, and every Cargo command in every worktree then failed
# with "No such file or directory". The Cargo entry points now rebuild a
# missing wrapper in place. This drives the real entry point against a
# fixture checkout whose config names a deleted wrapper.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

# shellcheck source=scripts/lib/cargo_env.sh
source "$repo_root/scripts/lib/cargo_env.sh"

checkout="$tmp_root/checkout"
storage="$tmp_root/storage"
mkdir -p "$checkout/scripts/lib" "$checkout/.cargo"
cp "$repo_root/scripts/sccache_rustc_wrapper.rs" "$checkout/scripts/"
cp "$repo_root/scripts/lib/"*.sh "$checkout/scripts/lib/"
cp "$repo_root/scripts/cargo_with_worktree_build_dir.sh" "$checkout/scripts/"
cp "$repo_root/rust-toolchain.toml" "$checkout/" 2>/dev/null || true
printf '[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n[workspace]\n' >"$checkout/Cargo.toml"
mkdir -p "$checkout/src" && printf 'fn main() {}\n' >"$checkout/src/main.rs"

wrapper=$(harn_sccache_wrapper_path "$checkout" "$storage")

# The rebuilt wrapper ends in an exec of a program named `sccache`, and the
# repository-policy lane installs no compiler cache. Stand up a forwarding stub
# there rather than skipping, as sccache_rustc_wrapper_test.sh does.
if ! command -v sccache >/dev/null 2>&1; then
  mkdir -p "$tmp_root/stub-bin"
  printf '#!/usr/bin/env bash\n# Stands in for sccache: run the compiler, cache nothing.\nexec "$@"\n' \
    >"$tmp_root/stub-bin/sccache"
  chmod +x "$tmp_root/stub-bin/sccache"
  PATH="$tmp_root/stub-bin:$PATH"
fi
write_config() {
  printf '[build]\nrustc-wrapper = "%s"\ntarget-dir = "%s" # harn-dev-setup-managed\n' \
    "$1" "$tmp_root/target" >"$checkout/.cargo/config.toml"
}

# 1. A configured, deleted wrapper is rebuilt by the real Cargo entry point,
#    and the build that needed it succeeds.
write_config "$wrapper"
[[ ! -e "$wrapper" ]] || fail "fixture wrapper exists before the run"
if ! (cd "$checkout" && HARN_CARGO_LEASE_MODE=off ./scripts/cargo_with_worktree_build_dir.sh build --quiet) \
  >"$tmp_root/build.log" 2>&1; then
  cat "$tmp_root/build.log" >&2
  fail "the build through a deleted wrapper did not recover"
fi
[[ -x "$wrapper" ]] || fail "the configured wrapper was not rebuilt: $wrapper"
grep -q 'rebuilding the missing rustc wrapper' "$tmp_root/build.log" \
  || fail "the rebuild did not say what it restored"

# 2. A wrapper from an older source cannot be rebuilt in place. The entry point
#    refuses with the regenerating command instead of building something else.
stale="$storage/sccache-wrapper/fixture-triple-0000/harn-sccache-wrapper"
write_config "$stale"
if (cd "$checkout" && HARN_CARGO_LEASE_MODE=off ./scripts/cargo_with_worktree_build_dir.sh build --quiet) \
  >"$tmp_root/stale.log" 2>&1; then
  fail "a stale wrapper path was accepted"
fi
grep -q 'make setup' "$tmp_root/stale.log" || fail "the stale refusal named no remedy"
[[ ! -e "$stale" ]] || fail "a stale wrapper path was fabricated"

# 3. A wrapper dev setup does not manage is never touched.
write_config "$tmp_root/elsewhere/sccache"
harn_restore_configured_sccache_wrapper "$checkout" || fail "an unmanaged wrapper path was refused"
[[ ! -e "$tmp_root/elsewhere" ]] || fail "an unmanaged wrapper path was created"

echo "sccache wrapper restore: ok"
