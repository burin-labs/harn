#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

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
  printf '#!/usr/bin/env bash\nset -euo pipefail\nexit 0\n' > "$repo/bin/git"
  chmod +x "$repo/bin/cargo" "$repo/bin/git"
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
  PATH="$repo/bin:/usr/bin:/bin" \
    HOME="$tmp_root/home-$profile" \
    XDG_CACHE_HOME="$tmp_root/cache-$profile" \
    TMPDIR="$tmp_root/tmp-$profile" \
    HARN_DEV_SETUP_PROFILE="$profile" \
    HARN_DEV_SETUP_FORCE=1 \
    HARN_DEV_SETUP_STATE_DIR="$tmp_root/state-$profile" \
    HARN_DEV_TARGET_WORKTREE_PATH="$repo" \
    DEV_SETUP_TEST_CARGO_RECORD="$cargo_record" \
    "$repo/scripts/dev_setup.sh" > "$output" 2>&1
}

rust_repo=$(make_fixture_repo rust)
rust_cargo="$tmp_root/rust-cargo.txt"
run_setup "$rust_repo" rust "$tmp_root/rust-output.txt" "$rust_cargo"

if ! grep -Fxq 'check --workspace' "$rust_cargo"; then
  echo "rust setup did not run the workspace check" >&2
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
rust_storage_root="$tmp_root/cache-rust/harn/dev-setup"
rust_target_dir="$rust_storage_root/harn-target/$(basename "$tmp_root")-rust"
if ! grep -Fxq "target-dir = \"$rust_target_dir\" # harn-dev-setup-managed" "$rust_repo/.cargo/config.toml"; then
  echo "rust setup did not use the durable per-worktree target directory" >&2
  exit 1
fi
if ! grep -Fxq "build-dir = \"$rust_storage_root/cargo-build-shared\" # harn-dev-setup-managed" "$rust_repo/.cargo/config.toml"; then
  echo "rust setup did not use the durable shared build directory" >&2
  exit 1
fi
prune_output="$(
  HARN_DEV_SETUP_STORAGE_ROOT="$rust_storage_root" \
    HARN_TARGET_GC_ROOTS="$tmp_root/no-repos" \
    "$repo_root/scripts/prune_stale_targets.sh" --dry-run
)"
if ! grep -Fq "roots=$rust_storage_root/harn-target" <<< "$prune_output"; then
  echo "stale-target pruning did not use the setup storage root" >&2
  exit 1
fi
default_prune_output="$(
  HOME="$tmp_root/home-prune-default" \
    XDG_CACHE_HOME="$tmp_root/cache-rust" \
    TMPDIR="$tmp_root/tmp-rust" \
    HARN_TARGET_GC_ROOTS="$tmp_root/no-repos" \
    "$repo_root/scripts/prune_stale_targets.sh" --dry-run
)"
if ! grep -Fq "roots=$rust_storage_root/harn-target" <<< "$default_prune_output"; then
  echo "default stale-target pruning did not discover the Rust setup cache root" >&2
  exit 1
fi

add_available_cargo_tools "$rust_repo"
mkdir -p "$tmp_root/tmp-profile-switch"
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
  echo "profile switch left a generated shared build directory in Cargo config" >&2
  exit 1
fi

user_repo=$(make_fixture_repo user-config)
mkdir -p "$user_repo/.cargo"
printf '%s\n' '[build]' 'target-dir = "/mnt/team/harn-target/release"' 'build-dir = "/mnt/team/cargo-build-shared"' > "$user_repo/.cargo/config.toml"
add_available_cargo_tools "$user_repo"
mkdir -p "$tmp_root/tmp-user-config"
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

legacy_repo=$(make_fixture_repo legacy-config)
mkdir -p "$legacy_repo/.cargo"
printf '%s\n' '[build]' 'target-dir = "/tmp/harn-target/legacy"' 'build-dir = "/tmp/cargo-build-shared"' > "$legacy_repo/.cargo/config.toml"
add_available_cargo_tools "$legacy_repo"
mkdir -p "$tmp_root/tmp-legacy-config"
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
if ! grep -Fq "HARN_DEV_SETUP_PROFILE must be 'full' or 'rust'" "$tmp_root/invalid-output.txt"; then
  echo "invalid setup profile did not explain the accepted values" >&2
  exit 1
fi

echo "dev_setup_profile_test: ok"
