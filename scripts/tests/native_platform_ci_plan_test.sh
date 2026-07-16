#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/native_platform_ci_plan.sh"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

workflow="$tmp_dir/ci.yml"
cat > "$workflow" <<'YAML'
name: CI
jobs:
  package-audit:
    steps:
      - run: make package-audit
  windows-plan:
    steps:
      - run: echo windows route
  windows:
    steps:
      - run: cargo test --target windows
  macos-plan:
    steps:
      - run: echo macos route
  macos:
    steps:
      - run: cargo clippy
YAML

write_paths() {
  local name="$1"
  shift
  printf '%s\n' "$@" > "$tmp_dir/$name"
}

write_diff() {
  local name="$1"
  local hunk="$2"
  cat > "$tmp_dir/$name" <<EOF
diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
index old..new 100644
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
$hunk
EOF
}

plan() {
  "$script" --workflow "$workflow" "$@"
}

assert_plan() {
  local expected="$1"
  shift
  local actual
  actual="$(plan "$@")"
  if [[ "$actual" != "$expected" ]]; then
    echo "expected $expected, got $actual for: $*" >&2
    exit 1
  fi
}

write_paths docs docs/readme.md
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/docs"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/docs"

write_paths windows_source crates/harn-hostlib/src/lib.rs
assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/windows_source"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/windows_source"

write_paths release_meta Cargo.lock changelog.d/123.fixed.md
assert_plan false --platform windows --event push --changed-files "$tmp_dir/release_meta"
assert_plan false --platform macos --event push --changed-files "$tmp_dir/release_meta"

write_paths release_workflow .github/workflows/build-release-binaries.yml
assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/release_workflow"
assert_plan true --platform macos --event pull_request --changed-files "$tmp_dir/release_workflow"

write_paths ci_only .github/workflows/ci.yml
write_diff package.diff '@@ -4,1 +4,1 @@
-      - run: make package-audit
+      - run: make package-audit-fast'
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/package.diff"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/package.diff"

write_diff windows.diff '@@ -7,1 +7,1 @@
-      - run: echo windows route
+      - run: echo windows route v2'
assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/windows.diff"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/windows.diff"

write_diff macos.diff '@@ -13,1 +13,1 @@
-      - run: echo macos route
+      - run: echo macos route v2'
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/macos.diff"
assert_plan true --platform macos --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/macos.diff"

assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/missing.diff"

echo "native_platform_ci_plan_test: ok"
