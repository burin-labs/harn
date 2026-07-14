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
mkdir -p "$fake_bin" "$work/.githooks" "$work/crates/harn-lint/src"

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
members = ["crates/harn-lint"]
resolver = "2"
TOML
cat > "$work/crates/harn-lint/Cargo.toml" <<'TOML'
[package]
name = "harn-lint"
version = "0.0.0"
edition = "2021"
TOML
printf 'pub fn base() {}\n' > "$work/crates/harn-lint/src/lib.rs"

git -C "$work" config user.email "test@example.com"
git -C "$work" config user.name "Test User"
git -C "$work" config commit.gpgsign false
git -C "$work" remote add origin "$origin"
git -C "$work" add .
git -C "$work" commit --quiet -m base
git -C "$work" branch -M main
git -C "$work" push --quiet -u origin main
git -C "$work" checkout --quiet -b feature

printf 'pub fn base() {}\npub fn changed() {}\n' > "$work/crates/harn-lint/src/lib.rs"
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

echo "hook_rust_gate_test: ok"
