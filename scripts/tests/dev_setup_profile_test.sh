#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

# Every case below decides for itself whether a per-worktree target dir is
# configured, so neither worktree-path variable may reach a fixture from the
# ambient environment. Both are unset in CI and exported in exactly the
# developer and agent shells this suite exists to protect, which is how a case
# that assumes "no per-worktree target dir" can pass in CI and fail locally.
# Cases that want one set it explicitly.
export HARN_DEV_TARGET_WORKTREE_PATH=
export CODEX_WORKTREE_PATH=

make_fixture_repo() {
  local name="$1"
  local repo="$tmp_root/$name"

  mkdir -p "$repo/scripts" "$repo/bin"
  cp "$repo_root/scripts/dev_setup.sh" "$repo/scripts/dev_setup.sh"
  chmod +x "$repo/scripts/dev_setup.sh"
  printf '[package]\nname = "setup-fixture"\nversion = "0.1.0"\n' > "$repo/Cargo.toml"
  printf '#!/usr/bin/env bash\nset -euo pipefail\n' > "$repo/scripts/configure_merge_drivers.sh"
  printf '#!/usr/bin/env bash\nset -euo pipefail\n' > "$repo/scripts/sign_local_macos.sh"
  chmod +x "$repo/scripts/configure_merge_drivers.sh" "$repo/scripts/sign_local_macos.sh"
  {
    printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail'
    printf '%s\n' 'printf "%s\\n" "$*" >> "$DEV_SETUP_TEST_CARGO_RECORD"'
  } > "$repo/bin/cargo"
  for tool in git go; do
    printf '#!/usr/bin/env bash\nset -euo pipefail\nexit 0\n' > "$repo/bin/$tool"
  done
  chmod +x "$repo/bin/cargo" "$repo/bin/git" "$repo/bin/go"
  printf '%s\n' "$repo"
}

add_available_cargo_tools() {
  local repo="$1"

  for tool in cargo-nextest sccache; do
    printf '#!/usr/bin/env bash\nset -euo pipefail\nexit 0\n' > "$repo/bin/$tool"
    chmod +x "$repo/bin/$tool"
  done
}

run_setup() {
  local repo="$1"
  local profile="$2"
  local output="$3"
  local cargo_record="$4"

  mkdir -p "$tmp_root/tmp-$profile"
  HARN_DEV_SETUP_STORAGE_ROOT= \
  HARN_DEV_TARGET_DIR= \
  HARN_DEV_BUILD_DIR= \
  PATH="$repo/bin:/usr/bin:/bin" \
    HOME="$tmp_root/home-$profile" \
    XDG_CACHE_HOME="$tmp_root/cache-$profile" \
    TMPDIR="$tmp_root/tmp-$profile" \
    HARN_DEV_SETUP_PROFILE="$profile" \
    HARN_DEV_SETUP_FORCE=1 \
    HARN_DEV_SETUP_STATE_DIR="$tmp_root/state-$profile" \
    HARN_DEV_TARGET_WORKTREE_PATH="${SETUP_TEST_WORKTREE_PATH:-$repo}" \
    DEV_SETUP_TEST_CARGO_RECORD="$cargo_record" \
    "$repo/scripts/dev_setup.sh" > "$output" 2>&1
}

rust_repo=$(make_fixture_repo rust)
add_available_cargo_tools "$rust_repo"
rust_cargo="$tmp_root/rust-cargo.txt"
run_setup "$rust_repo" rust "$tmp_root/rust-output.txt" "$rust_cargo"

if ! grep -Fxq 'check --locked --workspace' "$rust_cargo"; then
  echo "rust setup did not run the locked workspace check" >&2
  exit 1
fi
if grep -Fq 'install ' "$rust_cargo"; then
  echo "rust setup installed an optional tool" >&2
  exit 1
fi
if [[ -e "$rust_repo/node_modules" || -e "$rust_repo/crates/harn-cli/portal/node_modules" ]]; then
  echo "rust setup installed frontend dependencies" >&2
  exit 1
fi

bootstrap_repo=$(make_fixture_repo bootstrap)
add_available_cargo_tools "$bootstrap_repo"
bootstrap_cargo="$tmp_root/bootstrap-cargo.txt"
run_setup "$bootstrap_repo" bootstrap "$tmp_root/bootstrap-output.txt" "$bootstrap_cargo"
if [[ -s "$bootstrap_cargo" ]]; then
  echo "bootstrap setup invoked Cargo instead of deferring compilation" >&2
  exit 1
fi
if ! grep -Fq 'deferring compilation to the final task lane' "$tmp_root/bootstrap-output.txt"; then
  echo "bootstrap setup did not report its deferred build" >&2
  exit 1
fi
bootstrap_target_dir="$tmp_root/cache-bootstrap/harn/dev-setup/harn-target/$(basename "$tmp_root")-bootstrap"
if ! grep -Fxq "target-dir = \"$bootstrap_target_dir\" # harn-dev-setup-managed" \
  "$bootstrap_repo/.cargo/config.toml"; then
  echo "bootstrap setup did not configure a durable private target directory" >&2
  exit 1
fi

no_sccache_repo=$(make_fixture_repo no-sccache)
run_setup "$no_sccache_repo" bootstrap "$tmp_root/no-sccache-output.txt" "$tmp_root/no-sccache-cargo.txt"
if grep -Eq 'rustc-wrapper|SCCACHE_BASEDIRS' "$no_sccache_repo/.cargo/config.toml"; then
  echo "bootstrap setup configured sccache when it was unavailable" >&2
  exit 1
fi

rust_storage_root="$tmp_root/cache-rust/harn/dev-setup"
rust_target_dir="$rust_storage_root/harn-target/$(basename "$tmp_root")-rust"
if ! grep -Fxq "target-dir = \"$rust_target_dir\" # harn-dev-setup-managed" "$rust_repo/.cargo/config.toml"; then
  echo "rust setup did not use the durable per-worktree target directory" >&2
  exit 1
fi
if ! grep -Fxq \
  'SCCACHE_BASEDIRS = { value = ".", relative = true, force = true } # harn-dev-setup-managed' \
  "$rust_repo/.cargo/config.toml"; then
  echo "rust setup did not normalize sccache paths across worktrees" >&2
  exit 1
fi
if grep -Eq '^[[:space:]]*build-dir[[:space:]]*=' "$rust_repo/.cargo/config.toml"; then
  echo "rust setup shared Cargo build scratch across worktrees" >&2
  exit 1
fi
awk -v build_dir="$rust_storage_root/cargo-build-shared" '
  /^\[env\][[:space:]]*$/ {
    printf "build-dir = \"%s\" # harn-dev-setup-managed\n", build_dir
  }
  { print }
' "$rust_repo/.cargo/config.toml" > "$rust_repo/.cargo/config.toml.tmp"
mv "$rust_repo/.cargo/config.toml.tmp" "$rust_repo/.cargo/config.toml"
run_setup "$rust_repo" rust "$tmp_root/rust-migration-output.txt" "$rust_cargo"
if grep -Eq '^[[:space:]]*build-dir[[:space:]]*=' "$rust_repo/.cargo/config.toml"; then
  echo "rust setup did not remove the legacy shared build directory" >&2
  exit 1
fi
if ! grep -Fxq "target-dir = \"$rust_target_dir\" # harn-dev-setup-managed" "$rust_repo/.cargo/config.toml"; then
  echo "shared-build migration did not preserve the per-worktree target directory" >&2
  exit 1
fi
prune_output="$(
  HARN_DEV_SETUP_STORAGE_ROOT="$rust_storage_root" \
    HARN_TARGET_GC_ROOTS="$tmp_root/no-repos" \
    "$repo_root/scripts/prune_stale_targets.sh"
)"
if ! grep -Fq "roots=$rust_storage_root/harn-target" <<< "$prune_output"; then
  echo "stale-target pruning did not use the setup storage root" >&2
  exit 1
fi
if [[ ! -d "$rust_target_dir" ]]; then
  echo "stale-target pruning removed a recently active target" >&2
  exit 1
fi
default_prune_output="$(
  HOME="$tmp_root/home-prune-default" \
    XDG_CACHE_HOME="$tmp_root/cache-rust" \
    TMPDIR="$tmp_root/tmp-rust" \
    HARN_TARGET_GC_ROOTS="$tmp_root/no-repos" \
    "$repo_root/scripts/prune_stale_targets.sh"
)"
if ! grep -Fq "roots=$rust_storage_root/harn-target" <<< "$default_prune_output"; then
  echo "default stale-target pruning did not discover the Rust setup cache root" >&2
  exit 1
fi
if [[ ! -d "$rust_target_dir" ]]; then
  echo "default stale-target pruning removed a recently active target" >&2
  exit 1
fi

add_available_cargo_tools "$rust_repo"
mkdir -p "$tmp_root/tmp-profile-switch"
HARN_DEV_SETUP_STORAGE_ROOT= \
HARN_DEV_TARGET_DIR= \
HARN_DEV_BUILD_DIR= \
PATH="$rust_repo/bin:/usr/bin:/bin" \
  HOME="$tmp_root/home-profile-switch" \
  TMPDIR="$tmp_root/tmp-profile-switch" \
  HARN_DEV_SETUP_PROFILE=full \
  HARN_DEV_SETUP_STORAGE_ROOT="$tmp_root/full-storage-root" \
  HARN_DEV_SETUP_FORCE=1 \
  HARN_DEV_SETUP_STATE_DIR="$tmp_root/state-profile-switch" \
  DEV_SETUP_TEST_CARGO_RECORD="$tmp_root/profile-switch-cargo.txt" \
  "$rust_repo/scripts/dev_setup.sh" > "$tmp_root/profile-switch-output.txt" 2>&1
if grep -Eq '^[[:space:]]*target-dir[[:space:]]*=' "$rust_repo/.cargo/config.toml"; then
  echo "profile switch left a generated target directory in Cargo config" >&2
  exit 1
fi
if grep -Eq '^[[:space:]]*build-dir[[:space:]]*=' "$rust_repo/.cargo/config.toml"; then
  echo "profile switch left a generated build directory in Cargo config" >&2
  exit 1
fi

override_repo=$(make_fixture_repo build-dir-override)
add_available_cargo_tools "$override_repo"
mkdir -p "$tmp_root/tmp-build-dir-override"
PATH="$override_repo/bin:/usr/bin:/bin" \
  HOME="$tmp_root/home-build-dir-override" \
  TMPDIR="$tmp_root/tmp-build-dir-override" \
  HARN_DEV_SETUP_PROFILE=full \
  HARN_DEV_SETUP_FORCE=1 \
  HARN_DEV_SETUP_STATE_DIR="$tmp_root/state-build-dir-override" \
  HARN_DEV_BUILD_DIR="$tmp_root/operator-build-dir" \
  DEV_SETUP_TEST_CARGO_RECORD="$tmp_root/build-dir-override-cargo.txt" \
  "$override_repo/scripts/dev_setup.sh" > "$tmp_root/build-dir-override-output.txt" 2>&1
if ! grep -Fxq \
  "build-dir = \"$tmp_root/operator-build-dir\" # harn-dev-setup-managed" \
  "$override_repo/.cargo/config.toml"; then
  echo "setup did not preserve the explicit Cargo build-dir override" >&2
  exit 1
fi

user_repo=$(make_fixture_repo user-config)
mkdir -p "$user_repo/.cargo"
printf '%s\n' \
  '[build]' \
  'target-dir = "/mnt/team/harn-target/release"' \
  'build-dir = "/mnt/team/cargo-build-shared"' \
  '[env]' \
  'SCCACHE_BASEDIRS = "/mnt/team/source"' \
  > "$user_repo/.cargo/config.toml"
add_available_cargo_tools "$user_repo"
mkdir -p "$tmp_root/tmp-user-config"
HARN_DEV_SETUP_STORAGE_ROOT= \
HARN_DEV_TARGET_DIR= \
HARN_DEV_BUILD_DIR= \
PATH="$user_repo/bin:/usr/bin:/bin" \
  HOME="$tmp_root/home-user-config" \
  TMPDIR="$tmp_root/tmp-user-config" \
  HARN_DEV_SETUP_PROFILE=full \
  HARN_DEV_SETUP_FORCE=1 \
  HARN_DEV_SETUP_STATE_DIR="$tmp_root/state-user-config" \
  DEV_SETUP_TEST_CARGO_RECORD="$tmp_root/user-config-cargo.txt" \
  "$user_repo/scripts/dev_setup.sh" > "$tmp_root/user-config-output.txt" 2>&1
if ! grep -Fxq 'target-dir = "/mnt/team/harn-target/release"' "$user_repo/.cargo/config.toml"; then
  echo "setup rewrote a user-owned target directory" >&2
  exit 1
fi
if ! grep -Fxq 'build-dir = "/mnt/team/cargo-build-shared"' "$user_repo/.cargo/config.toml"; then
  echo "setup rewrote a user-owned build directory" >&2
  exit 1
fi
if ! grep -Fxq 'SCCACHE_BASEDIRS = "/mnt/team/source"' "$user_repo/.cargo/config.toml"; then
  echo "setup rewrote a user-owned sccache base directory" >&2
  exit 1
fi

legacy_repo=$(make_fixture_repo legacy-config)
mkdir -p "$legacy_repo/.cargo"
printf '%s\n' '[build]' 'target-dir = "/tmp/harn-target/legacy"' 'build-dir = "/tmp/cargo-build-shared"' > "$legacy_repo/.cargo/config.toml"
add_available_cargo_tools "$legacy_repo"
mkdir -p "$tmp_root/tmp-legacy-config"
HARN_DEV_SETUP_STORAGE_ROOT= \
HARN_DEV_TARGET_DIR= \
HARN_DEV_BUILD_DIR= \
PATH="$legacy_repo/bin:/usr/bin:/bin" \
  HOME="$tmp_root/home-legacy-config" \
  TMPDIR="$tmp_root/tmp-legacy-config" \
  HARN_DEV_SETUP_PROFILE=full \
  HARN_DEV_SETUP_FORCE=1 \
  HARN_DEV_SETUP_STATE_DIR="$tmp_root/state-legacy-config" \
  DEV_SETUP_TEST_CARGO_RECORD="$tmp_root/legacy-config-cargo.txt" \
  "$legacy_repo/scripts/dev_setup.sh" > "$tmp_root/legacy-config-output.txt" 2>&1
if grep -Eq '^[[:space:]]*(target-dir|build-dir)[[:space:]]*=' "$legacy_repo/.cargo/config.toml"; then
  echo "setup did not remove a legacy generated Cargo configuration" >&2
  exit 1
fi

full_repo=$(make_fixture_repo full)
full_cargo="$tmp_root/full-cargo.txt"
tool_target="$tmp_root/durable-cargo-install"
HARN_DEV_SETUP_TOOL_TARGET_DIR="$tool_target" \
  run_setup "$full_repo" full "$tmp_root/full-output.txt" "$full_cargo"

if ! grep -Fxq "install --target-dir $tool_target cargo-nextest --locked" "$full_cargo"; then
  echo "full setup did not route cargo-nextest through the durable tool target" >&2
  exit 1
fi
if ! grep -Fxq "install --target-dir $tool_target sccache --locked" "$full_cargo"; then
  echo "full setup did not route sccache through the durable tool target" >&2
  exit 1
fi

invalid_repo=$(make_fixture_repo invalid)
invalid_cargo="$tmp_root/invalid-cargo.txt"
if run_setup "$invalid_repo" unsupported "$tmp_root/invalid-output.txt" "$invalid_cargo"; then
  echo "invalid setup profile unexpectedly succeeded" >&2
  exit 1
fi
if [[ -s "$invalid_cargo" ]]; then
  echo "invalid setup profile started dependency work" >&2
  exit 1
fi
if ! grep -Fq "HARN_DEV_SETUP_PROFILE must be 'full', 'rust', or 'bootstrap'" "$tmp_root/invalid-output.txt"; then
  echo "invalid setup profile did not explain the accepted values" >&2
  exit 1
fi

# A worktree path naming some other checkout must not decide this checkout's
# target dir: an agent session exporting its primary checkout path, or a value
# inherited from a parent shell, would otherwise hand two worktrees the same
# mutable target dir.
foreign_repo=$(make_fixture_repo foreign)
mismatch_repo=$(make_fixture_repo mismatch)
mismatch_cargo="$tmp_root/mismatch-cargo.txt"
SETUP_TEST_WORKTREE_PATH="$foreign_repo" \
  run_setup "$mismatch_repo" rust "$tmp_root/mismatch-output.txt" "$mismatch_cargo"

mismatch_storage_root="$tmp_root/cache-rust/harn/dev-setup"
foreign_target_dir="$mismatch_storage_root/harn-target/$(basename "$tmp_root")-foreign"
mismatch_target_dir="$mismatch_storage_root/harn-target/$(basename "$tmp_root")-mismatch"

if grep -Fq "target-dir = \"$foreign_target_dir\"" "$mismatch_repo/.cargo/config.toml"; then
  echo "setup adopted a target dir belonging to another checkout" >&2
  exit 1
fi
if ! grep -Fxq "target-dir = \"$mismatch_target_dir\" # harn-dev-setup-managed" "$mismatch_repo/.cargo/config.toml"; then
  echo "setup did not fall back to this checkout's own target directory" >&2
  exit 1
fi
if ! grep -Fq "does not name this checkout" "$tmp_root/mismatch-output.txt"; then
  echo "setup did not warn that the configured worktree path was ignored" >&2
  exit 1
fi

echo "dev_setup_profile_test: ok"
