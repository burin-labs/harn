#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

origin="$tmp_root/origin.git"
work="$tmp_root/work"
fake_bin="$tmp_root/bin"
record="$tmp_root/cargo-record.txt"
real_git=$(command -v git)

git init --bare --quiet "$origin"
git init --quiet "$work"
mkdir -p \
  "$fake_bin" \
  "$work/.githooks" \
  "$work/crates/harn-lint/src" \
  "$work/crates/harn-other/src" \
  "$work/crates/harn-vm/src/llm/capability_sources/20-providers" \
  "$work/spec/provider-catalog"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo %s\n' "$*" >> "${FAKE_CARGO_RECORD:?FAKE_CARGO_RECORD is required}"
SH
chmod +x "$fake_bin/cargo"

cat > "$fake_bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "log -1 --format=%G? "* ]]; then
  printf 'G\n'
  exit 0
fi
exec "${REAL_GIT:?REAL_GIT is required}" "$@"
SH
chmod +x "$fake_bin/git"

cat > "$fake_bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
exit 0
SH
chmod +x "$fake_bin/gh"

cat > "$fake_bin/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'harn %s\n' "$*" >> "${FAKE_CARGO_RECORD:?FAKE_CARGO_RECORD is required}"
SH
chmod +x "$fake_bin/harn"
export HARN_BIN="$fake_bin/harn"

cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'make %s\n' "$*" >> "${FAKE_CARGO_RECORD:?FAKE_CARGO_RECORD is required}"
SH
chmod +x "$fake_bin/make"

cp "$repo_root/.githooks/lib.sh" "$work/.githooks/lib.sh"
cp "$repo_root/.githooks/pre-commit" "$work/.githooks/pre-commit"
cp "$repo_root/.githooks/pre-push" "$work/.githooks/pre-push"
chmod +x "$work/.githooks/pre-commit" "$work/.githooks/pre-push"

cat > "$work/Cargo.toml" <<'TOML'
[workspace]
members = ["crates/harn-lint", "crates/harn-other", "crates/harn-vm"]
resolver = "2"
TOML
cat > "$work/crates/harn-lint/Cargo.toml" <<'TOML'
[package]
name = "harn-lint"
version = "0.0.0"
edition = "2021"
TOML
cat > "$work/crates/harn-other/Cargo.toml" <<'TOML'
[package]
name = "harn-other"
version = "0.0.0"
edition = "2021"
TOML
cat > "$work/crates/harn-vm/Cargo.toml" <<'TOML'
[package]
name = "harn-vm"
version = "0.0.0"
edition = "2021"
TOML
printf 'pub mod obsolete;\npub fn base() {}\n' > "$work/crates/harn-lint/src/lib.rs"
printf 'pub fn removable() {}\n' > "$work/crates/harn-lint/src/obsolete.rs"
printf 'pub fn other() {}\n' > "$work/crates/harn-other/src/lib.rs"
printf 'base = true\n' > "$work/crates/harn-vm/src/llm/capabilities.toml"
printf 'base = true\n' > "$work/crates/harn-vm/src/llm/capability_sources/20-providers/20-local-ollama.toml"
printf '{}\n' > "$work/spec/provider-catalog/provider-catalog.json"

git -C "$work" config user.email "test@example.com"
git -C "$work" config user.name "Test User"
git -C "$work" config commit.gpgsign false
git -C "$work" remote add origin "$origin"
git -C "$work" add .
git -C "$work" commit --quiet -m base
git -C "$work" branch -M main
git -C "$work" push --quiet -u origin main
git -C "$work" checkout --quiet -b feature

printf 'pub mod obsolete;\npub fn base() {}\npub fn changed() {}\n' > "$work/crates/harn-lint/src/lib.rs"
git -C "$work" add crates/harn-lint/src/lib.rs

: > "$record"
(
  cd "$work"
  CARGO_BUILD_BUILD_DIR="$tmp_root/build" \
    CARGO_TARGET_DIR="$tmp_root/target" \
    FAKE_CARGO_RECORD="$record" \
    HOOK_TIMING_LOG_DIR="$tmp_root/timings" \
    PATH="$fake_bin:$PATH" \
    REAL_GIT="$real_git" \
    ./.githooks/pre-commit >/dev/null
)

cat > "$tmp_root/expected-pre-commit.txt" <<'EOF'
cargo fmt --all
EOF
if ! diff -u "$tmp_root/expected-pre-commit.txt" "$record"; then
  echo "pre-commit should format Rust without compiling it" >&2
  exit 1
fi

git -C "$work" commit --quiet --no-verify -m feature
local_sha=$(git -C "$work" rev-parse HEAD)
remote_sha=$(git -C "$work" rev-parse origin/main)

: > "$record"
(
  cd "$work"
  printf 'refs/heads/feature %s refs/heads/feature %s\n' "$local_sha" "$remote_sha" \
    | CARGO_BUILD_BUILD_DIR="$tmp_root/build" \
      CARGO_TARGET_DIR="$tmp_root/target" \
      FAKE_CARGO_RECORD="$record" \
      HOOK_TIMING_LOG_DIR="$tmp_root/timings" \
      PATH="$fake_bin:$PATH" \
      REAL_GIT="$real_git" \
      ./.githooks/pre-push origin "$origin" >/dev/null
)

cat > "$tmp_root/expected-pre-push.txt" <<'EOF'
cargo clippy -p harn-lint --tests -- -D warnings
EOF
if ! diff -u "$tmp_root/expected-pre-push.txt" "$record"; then
  echo "pre-push should run one changed-package Rust lint/test compile" >&2
  exit 1
fi

: > "$record"
(
  cd "$work"
  printf 'refs/heads/feature %s refs/heads/feature %s\n' "$local_sha" "$remote_sha" \
    | CARGO_BUILD_BUILD_DIR="$tmp_root/build" \
      CARGO_TARGET_DIR="$tmp_root/target" \
      FAKE_CARGO_RECORD="$record" \
      HARN_PREPUSH_FULL_TESTS=1 \
      HOOK_TIMING_LOG_DIR="$tmp_root/timings" \
      PATH="$fake_bin:$PATH" \
      REAL_GIT="$real_git" \
      ./.githooks/pre-push origin "$origin" >/dev/null
)

cat > "$tmp_root/expected-full-pre-push.txt" <<'EOF'
cargo clippy -p harn-lint --tests -- -D warnings
make test
EOF
if ! diff -u "$tmp_root/expected-full-pre-push.txt" "$record"; then
  echo "full-test pre-push should preserve lint coverage before running the full suite" >&2
  exit 1
fi

git -C "$work" checkout --quiet -B provider-catalog-feature origin/main
printf 'base = false\n' > "$work/crates/harn-vm/src/llm/capabilities.toml"
printf 'native_tools = true\n' > "$work/crates/harn-vm/src/llm/capability_sources/20-providers/20-local-ollama.toml"
printf '{"models":[]}\n' > "$work/spec/provider-catalog/provider-catalog.json"
git -C "$work" add \
  crates/harn-vm/src/llm/capabilities.toml \
  crates/harn-vm/src/llm/capability_sources/20-providers/20-local-ollama.toml \
  spec/provider-catalog/provider-catalog.json
git -C "$work" commit --quiet --no-verify -m "update provider catalog data"
provider_catalog_sha=$(git -C "$work" rev-parse HEAD)

: > "$record"
(
  cd "$work"
  printf 'refs/heads/provider-catalog-feature %s refs/heads/provider-catalog-feature %s\n' \
    "$provider_catalog_sha" "$remote_sha" \
    | CARGO_BUILD_BUILD_DIR="$tmp_root/build" \
      CARGO_TARGET_DIR="$tmp_root/target" \
      FAKE_CARGO_RECORD="$record" \
      HOOK_TIMING_LOG_DIR="$tmp_root/timings" \
      PATH="$fake_bin:$PATH" \
      REAL_GIT="$real_git" \
      ./.githooks/pre-push origin "$origin" >/dev/null
)

cat > "$tmp_root/expected-provider-catalog-pre-push.txt" <<'EOF'
make -s check-provider-catalog
make -s check-provider-matrix
make -s check-provider-support
harn provider capabilities audit
EOF
if ! diff -u "$tmp_root/expected-provider-catalog-pre-push.txt" "$record"; then
  echo "provider catalog data-only pre-push should run catalog checks without Cargo" >&2
  exit 1
fi

# A deleted Rust path must still select its owning package. This deliberately
# strands `mod obsolete;`: real Cargo would reject the push, while the fake
# Cargo receipt proves the public hook invokes exactly the one compile gate.
git -C "$work" checkout --quiet -B deletion-feature origin/main
git -C "$work" rm --quiet crates/harn-lint/src/obsolete.rs
git -C "$work" commit --quiet --no-verify -m "delete obsolete module"
deletion_sha=$(git -C "$work" rev-parse HEAD)

: > "$record"
(
  cd "$work"
  printf 'refs/heads/deletion-feature %s refs/heads/deletion-feature %s\n' \
    "$deletion_sha" "$remote_sha" \
    | CARGO_BUILD_BUILD_DIR="$tmp_root/build" \
      CARGO_TARGET_DIR="$tmp_root/target" \
      FAKE_CARGO_RECORD="$record" \
      HOOK_TIMING_LOG_DIR="$tmp_root/timings" \
      PATH="$fake_bin:$PATH" \
      REAL_GIT="$real_git" \
      ./.githooks/pre-push origin "$origin" >/dev/null
)

if ! diff -u "$tmp_root/expected-pre-push.txt" "$record"; then
  echo "deletion-only Rust changes must run one owning-package compile gate" >&2
  exit 1
fi

# Rename detection normally makes --name-only report only the destination.
# Expanding a cross-crate rename into delete+add must select both owners: the
# source crate lost a module while the destination crate gained a Rust file.
git -C "$work" checkout --quiet -B rename-feature origin/main
git -C "$work" mv crates/harn-lint/src/obsolete.rs crates/harn-other/src/moved.rs
git -C "$work" commit --quiet --no-verify -m "move module across crates"
rename_sha=$(git -C "$work" rev-parse HEAD)

: > "$record"
(
  cd "$work"
  printf 'refs/heads/rename-feature %s refs/heads/rename-feature %s\n' \
    "$rename_sha" "$remote_sha" \
    | CARGO_BUILD_BUILD_DIR="$tmp_root/build" \
      CARGO_TARGET_DIR="$tmp_root/target" \
      FAKE_CARGO_RECORD="$record" \
      HOOK_TIMING_LOG_DIR="$tmp_root/timings" \
      PATH="$fake_bin:$PATH" \
      REAL_GIT="$real_git" \
      ./.githooks/pre-push origin "$origin" >/dev/null
)

cat > "$tmp_root/expected-rename-pre-push.txt" <<'EOF'
cargo clippy -p harn-lint -p harn-other --tests -- -D warnings
EOF
if ! diff -u "$tmp_root/expected-rename-pre-push.txt" "$record"; then
  echo "cross-crate Rust renames must compile both owning packages" >&2
  exit 1
fi

run_helper() {
  local changed=$1
  local packages="$tmp_root/changed-packages.txt"
  (
    cd "$work"
    # shellcheck source=/dev/null
    . .githooks/lib.sh
    FAKE_CARGO_RECORD="$record" PATH="$fake_bin:$PATH" \
      hook_run_rust_test_lint_gate "$changed" "$packages" >/dev/null
  )
}

printf 'Cargo.toml\n' > "$tmp_root/workspace-files.txt"
: > "$record"
run_helper "$tmp_root/workspace-files.txt"
if [[ "$(cat "$record")" != "cargo clippy --workspace --tests -- -D warnings" ]]; then
  echo "workspace changes should run one workspace Rust lint/test compile" >&2
  cat "$record" >&2
  exit 1
fi

printf 'build.rs\n' > "$tmp_root/root-rust-files.txt"
: > "$record"
run_helper "$tmp_root/root-rust-files.txt"
if [[ "$(cat "$record")" != "cargo clippy --workspace --tests -- -D warnings" ]]; then
  echo "unmapped Rust changes should fall back to the workspace gate" >&2
  cat "$record" >&2
  exit 1
fi

printf 'scripts/tool.sh\n' > "$tmp_root/non-crate-files.txt"
: > "$record"
run_helper "$tmp_root/non-crate-files.txt"
if [[ -s "$record" ]]; then
  echo "non-crate changes should not invoke Cargo" >&2
  cat "$record" >&2
  exit 1
fi

cat > "$tmp_root/provider-catalog-files.txt" <<'EOF'
crates/harn-vm/src/llm/capabilities.toml
crates/harn-vm/src/llm/capability_sources/20-providers/20-local-ollama.toml
spec/provider-catalog/provider-catalog.json
EOF
: > "$record"
run_helper "$tmp_root/provider-catalog-files.txt"
if [[ -s "$record" ]]; then
  echo "provider catalog data-only changes should not invoke Cargo" >&2
  cat "$record" >&2
  exit 1
fi

echo "hook_rust_gate_test: ok"
