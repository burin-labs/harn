#!/usr/bin/env bash
# Exercise the real narrow setup entry with fake compiler/configuration calls.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
setup_source=${1:-$repo_root/scripts/dev_setup.sh}
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
checkout="$tmp_root/checkout"
fixture_storage="$tmp_root/storage with spaces"
mkdir -p "$checkout/scripts/lib" "$checkout/.cargo" "$tmp_root/bin"
cp "$setup_source" "$checkout/scripts/dev_setup.sh"
cp "$repo_root/scripts/sccache_rustc_wrapper.rs" "$checkout/scripts/"
cp "$repo_root/scripts/lib/"*.sh "$checkout/scripts/lib/"
export REFRESH_CALLS="$tmp_root/calls"
export REFRESH_CHECKOUT="$checkout"
cat > "$tmp_root/bin/rustc" <<'SH'
#!/bin/bash
set -euo pipefail
if [[ "${1:-}" == -vV ]]; then
  printf 'host: fixture-target\n'
  exit
fi
printf 'compile\n' >> "$REFRESH_CALLS"
[[ "${REFRESH_FAIL:-0}" != 1 ]] || exit 73
while [[ "$#" -gt 0 ]]; do
  if [[ "$1" == -o ]]; then
    printf '#!/bin/sh\nexit 0\n' > "$2"
    chmod +x "$2"
    break
  fi
  shift
done
if [[ "${REFRESH_EDIT_CONFIG:-0}" == 1 ]]; then
  printf '# concurrent owner edit\n' >> "$REFRESH_CHECKOUT/.cargo/config.toml"
fi
if [[ "${REFRESH_EDIT_SOURCE:-0}" == 1 ]]; then
  printf '\n// source changed during compile\n' >> "$REFRESH_CHECKOUT/scripts/sccache_rustc_wrapper.rs"
fi
SH
cat > "$tmp_root/bin/forbidden" <<'SH'
#!/bin/sh
printf 'FORBIDDEN %s\n' "$0" >> "$REFRESH_CALLS"
exit 81
SH
chmod +x "$tmp_root/bin/rustc" "$tmp_root/bin/forbidden"
for name in git cargo rustup sccache npm; do
  ln -s forbidden "$tmp_root/bin/$name"
done
export PATH="$tmp_root/bin:/usr/bin:/bin"
# shellcheck source=scripts/lib/cargo_env.sh
source "$repo_root/scripts/lib/cargo_env.sh"
current=$(harn_sccache_wrapper_path "$checkout" "$fixture_storage")
old="$fixture_storage/sccache-wrapper/fixture-target-$(printf '%064d' 0)/harn-sccache-wrapper"
mkdir -p "$(dirname "$old")"
printf 'old source artifact\n' > "$old"
chmod +x "$old"
write_config() {
  cat > "$checkout/.cargo/config.toml" <<EOF
# Preserve formatting, targets and unrelated policy.
[build]
jobs = 6
rustc-wrapper = "$1"
target-dir = "/tmp/harn-target/keep" # harn-dev-setup-managed
build-dir = "/tmp/cargo-build-shared" # harn-dev-setup-managed
rustc-workspace-wrapper = "/existing/frontend"
[env]
SCCACHE_CACHE_SIZE = "24G"
EOF
}
run_refresh() {
  bash "$checkout/scripts/dev_setup.sh" --refresh-cargo-wrapper
}
write_config "$old"
chmod 640 "$checkout/.cargo/config.toml"
write_config "$current"
cp -p "$checkout/.cargo/config.toml" "$tmp_root/expected"
write_config "$old"
run_refresh > "$tmp_root/success.log" 2>&1 || {
  cat "$tmp_root/success.log" >&2
  fail 'the current wrapper was not refreshed'
}
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/expected" || fail 'unrelated config changed'
[[ -n "$(find "$checkout/.cargo/config.toml" -prune -perm 640)" ]] || fail 'config mode changed'
[[ -x "$current" ]] || fail 'the existing producer was not reached'
[[ "$(cat "$REFRESH_CALLS")" == compile ]] || fail 'unexpected setup or compiler calls'
[[ "$(cat "$old")" == 'old source artifact' ]] || fail 'the old artifact was overwritten'
[[ ! -e "$checkout/target" && ! -e "$checkout/.codex" ]] || fail 'setup created target/state'

cp -p "$checkout/.cargo/config.toml" "$tmp_root/idempotent"
run_refresh > "$tmp_root/idempotent.log" 2>&1
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/idempotent" || fail 'idempotent config changed'
[[ "$(cat "$REFRESH_CALLS")" == compile ]] || fail 'the current wrapper was recompiled'
if bash "$checkout/scripts/dev_setup.sh" --refresh-cargo-wraper > "$tmp_root/typo.log" 2>&1; then
  fail 'an unknown mode was accepted'
fi

# Missing config and user-owned wrappers refuse without creating setup state.
write_config "$tmp_root/user-wrapper"
cp "$checkout/.cargo/config.toml" "$tmp_root/unmanaged"
if run_refresh > "$tmp_root/unmanaged.log" 2>&1; then fail 'unmanaged wrapper accepted'; fi
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/unmanaged" || fail 'unmanaged config changed'
mv "$checkout/.cargo/config.toml" "$tmp_root/absent"
if run_refresh > "$tmp_root/absent.log" 2>&1; then fail 'absent config accepted'; fi
[[ ! -e "$checkout/.cargo/config.toml" ]] || fail 'absent config was created'

write_config "$old"
other_host="$fixture_storage/sccache-wrapper/other-target-$(printf '%064d' 0)/harn-sccache-wrapper"
write_config "$other_host"
cp "$checkout/.cargo/config.toml" "$tmp_root/other-host"
if run_refresh > "$tmp_root/other-host.log" 2>&1; then fail 'different host accepted'; fi
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/other-host" || fail 'different host config changed'
write_config "$old"
printf 'rustc-wrapper = "%s"\n' "$old" > "$tmp_root/duplicate"
sed '/^jobs =/r '"$tmp_root/duplicate" "$checkout/.cargo/config.toml" > "$tmp_root/duplicated-config"
cp "$tmp_root/duplicated-config" "$checkout/.cargo/config.toml"
if run_refresh > "$tmp_root/duplicate.log" 2>&1; then fail 'duplicate wrapper accepted'; fi
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/duplicated-config" || fail 'duplicate config changed'
write_config "$old"
mv "$checkout/.cargo/config.toml" "$tmp_root/symlink-config"
ln -s "$tmp_root/symlink-config" "$checkout/.cargo/config.toml"
if run_refresh > "$tmp_root/symlink.log" 2>&1; then fail 'symlink config accepted'; fi
[[ -L "$checkout/.cargo/config.toml" ]] || fail 'symlink config replaced'
rm "$checkout/.cargo/config.toml"
write_config "$old"

# A different source address forces the producer again, without deleting cache.
printf '\n// fixture source revision\n' >> "$checkout/scripts/sccache_rustc_wrapper.rs"
write_config "$old"
cp "$checkout/.cargo/config.toml" "$tmp_root/before-failure"
if REFRESH_FAIL=1 run_refresh > "$tmp_root/compiler-failure.log" 2>&1; then
  fail 'compiler failure accepted'
fi
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/before-failure" || fail 'failure changed config'
if REFRESH_EDIT_CONFIG=1 run_refresh > "$tmp_root/concurrent.log" 2>&1; then
  fail 'concurrent config edit accepted'
fi
grep -q 'concurrent owner edit' "$checkout/.cargo/config.toml" || fail 'owner edit lost'
grep -q 'changed during refresh' "$tmp_root/concurrent.log" || fail 'drift refusal missing'

# A source edit during compilation must not publish at the earlier address.
printf '\n// source mutation control\n' >> "$checkout/scripts/sccache_rustc_wrapper.rs"
unpublished=$(harn_sccache_wrapper_path "$checkout" "$fixture_storage")
[[ ! -e "$unpublished" ]] || fail 'mutation control artifact already exists'
write_config "$old"
cp "$checkout/.cargo/config.toml" "$tmp_root/before-source-edit"
if REFRESH_EDIT_SOURCE=1 run_refresh > "$tmp_root/source-edit.log" 2>&1; then
  fail 'source mutation during compile accepted'
fi
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/before-source-edit" || fail 'source edit changed config'
[[ ! -e "$unpublished" ]] || fail 'changed source published at the earlier address'

# A commented table ends [build]; its alias with the same name is unrelated.
commented_config() {
  write_config "$1"
  awk '/^\[build\]/ { print "[build] # preserve this header"; next }
       /^\[env\]/ { print "[alias] # preserve this table"; print "rustc-wrapper = \"check\"" } { print }' \
    "$checkout/.cargo/config.toml" > "$tmp_root/commented"
  cp "$tmp_root/commented" "$checkout/.cargo/config.toml"
}
current_after_edit=$(harn_sccache_wrapper_path "$checkout" "$fixture_storage")
commented_config "$current_after_edit"
cp "$checkout/.cargo/config.toml" "$tmp_root/commented-expected"
commented_config "$old"
run_refresh > "$tmp_root/commented.log" 2>&1 || fail 'commented table refresh failed'
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/commented-expected" || fail 'commented table alias changed'

# Setup accepts a relative storage root, resolved from this checkout.
relative_storage='relative cache'
relative_current=$(harn_sccache_wrapper_path "$checkout" "$relative_storage")
relative_old="$relative_storage/sccache-wrapper/fixture-target-$(printf '%064d' 0)/harn-sccache-wrapper"
mkdir -p "$checkout/$(dirname "$relative_old")"
printf 'relative old artifact\n' > "$checkout/$relative_old"
chmod +x "$checkout/$relative_old"
write_config "$relative_current"
cp "$checkout/.cargo/config.toml" "$tmp_root/relative-expected"
write_config "$relative_old"
run_refresh > "$tmp_root/relative.log" 2>&1 || {
  cat "$tmp_root/relative.log" >&2
  fail 'relative config refresh failed'
}
cmp -s "$checkout/.cargo/config.toml" "$tmp_root/relative-expected" || fail 'relative config changed'
[[ -x "$checkout/$relative_current" ]] || fail 'relative wrapper not produced in checkout'
[[ "$(cat "$checkout/$relative_old")" == 'relative old artifact' ]] || fail 'relative old artifact changed'
if grep -q FORBIDDEN "$REFRESH_CALLS"; then fail 'full setup or native build was reached'; fi
[[ "$(wc -l < "$REFRESH_CALLS")" -eq 6 ]] || fail 'refusals reached the compiler'

# Exercise the real resolver entry before its missing-wrapper bootstrap can run.
resolver_checkout="$tmp_root/resolver-checkout"
mkdir -p "$resolver_checkout/scripts/lib" "$resolver_checkout/.cargo"
cp "$repo_root/scripts/harn_bin.sh" "$resolver_checkout/scripts/"
cp "$repo_root/scripts/lib/"*.sh "$resolver_checkout/scripts/lib/"
cp "$checkout/scripts/sccache_rustc_wrapper.rs" "$resolver_checkout/scripts/"
printf '[package]\nname = "fixture"\nversion = "0.1.0"\n' > "$resolver_checkout/Cargo.toml"
resolver_wrapper=$(harn_sccache_wrapper_path "$resolver_checkout" "$tmp_root/resolver-storage")
printf '[build]\nrustc-wrapper = "%s"\n' "$resolver_wrapper" \
  > "$resolver_checkout/.cargo/config.toml"
cp "$resolver_checkout/.cargo/config.toml" "$tmp_root/resolver-config-before"
resolver_pin="$tmp_root/existing-harn"
cat > "$resolver_pin" <<'SH'
#!/bin/sh
printf 'runtime:%s\n' "$*" >> "$RESOLVER_RUNTIME_CALLS"
printf 'pinned-ok\n'
SH
chmod +x "$resolver_pin"
export RESOLVER_RUNTIME_CALLS="$tmp_root/resolver-runtime-calls"
: > "$REFRESH_CALLS"

(
  cd "$resolver_checkout"
  HARN_BIN="$resolver_pin" HARN_BIN_NO_BUILD=1 \
    bash scripts/harn_bin.sh -- --fixture-env-positive
) > "$tmp_root/resolver-env-positive.log" 2>&1 || {
  cat "$tmp_root/resolver-env-positive.log" >&2
  fail 'no-build could not execute an existing pinned runtime'
}
grep -Fxq 'runtime:--fixture-env-positive' "$RESOLVER_RUNTIME_CALLS" ||
  fail 'the existing runtime was not actually executed'
(
  cd "$resolver_checkout"
  HARN_BIN="$resolver_pin" HARN_BIN_NO_BUILD=0 \
    bash scripts/harn_bin.sh --no-build -- --fixture-flag-positive
) > "$tmp_root/resolver-flag-positive.log" 2>&1 || fail 'no-build flag lost the existing runtime'
grep -Fxq 'runtime:--fixture-flag-positive' "$RESOLVER_RUNTIME_CALLS" ||
  fail 'the no-build flag did not execute the runtime'

for runtime in missing stale; do
  if [[ "$runtime" == stale ]]; then
    mkdir -p "$resolver_checkout/target/debug"
    cp "$resolver_pin" "$resolver_checkout/target/debug/harn"
  fi
  if (
    cd "$resolver_checkout"
    HARN_BIN='' HARN_BIN_NO_BUILD=1 CARGO_TARGET_DIR="$resolver_checkout/target" \
      bash scripts/harn_bin.sh --print
  ) > "$tmp_root/resolver-$runtime.log" 2>&1; then
    fail "no-build accepted a $runtime worktree runtime"
  fi
  grep -Eq 'no fresh worktree harn binary|cannot prove worktree harn binary freshness' \
    "$tmp_root/resolver-$runtime.log" || fail "the $runtime refusal was not attributable"
done
if (
  cd "$resolver_checkout"
  HARN_BIN="$resolver_pin" HARN_BIN_NO_BUILD=invalid bash scripts/harn_bin.sh --print
) > "$tmp_root/resolver-invalid.log" 2>&1; then
  fail 'invalid no-build admission was accepted'
fi
grep -Fq 'HARN_BIN_NO_BUILD must be 0 or 1' "$tmp_root/resolver-invalid.log" ||
  fail 'invalid no-build admission lost its diagnostic'
[[ ! -s "$REFRESH_CALLS" ]] || fail 'no-build invoked compiler or tool bootstrap'
[[ ! -e "$resolver_wrapper" && ! -d "$(dirname "$resolver_wrapper")" ]] ||
  fail 'no-build restored the compiler wrapper cache'
[[ ! -e "$resolver_checkout/.cargo/harn-target-dir" ]] || fail 'no-build rewrote target discovery'
cmp -s "$resolver_checkout/.cargo/config.toml" "$tmp_root/resolver-config-before" ||
  fail 'no-build changed Cargo configuration'
echo 'cargo wrapper refresh: source-only controls passed'
