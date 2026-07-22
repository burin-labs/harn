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

write_policy_diff() {
  local name="$1"
  local path="$2"
  local hunk="$3"
  cat > "$tmp_dir/$name" <<EOF
diff --git a/$path b/$path
index old..new 100644
--- a/$path
+++ b/$path
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

write_paths native_hostlib_tests \
  crates/harn-hostlib/tests/harn_hostlib/secret_store_os_native.rs \
  crates/harn-hostlib/tests/harn_hostlib/sandbox_npm_offline_install.rs
assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/native_hostlib_tests"
assert_plan true --platform macos --event pull_request --changed-files "$tmp_dir/native_hostlib_tests"

write_paths windows_selector scripts/ci/affected_crate_args.sh
assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/windows_selector"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/windows_selector"

write_paths package_paths \
  crates/harn-modules/src/package_execution.rs \
  crates/harn-modules/src/package_imports.rs \
  crates/harn-modules/src/package_snapshot.rs
assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/package_paths"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/package_paths"

write_paths swift_protocol_paths \
  crates/harn-cli/src/commands/dump_protocol_artifacts/swift.rs \
  spec/protocol-artifacts/HarnProtocol.swift
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/swift_protocol_paths"
assert_plan true --platform macos --event pull_request --changed-files "$tmp_dir/swift_protocol_paths"

write_paths vm_macos_sandbox_test crates/harn-vm/tests/harn_vm/sandbox_hardened.rs
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/vm_macos_sandbox_test"
assert_plan true --platform macos --event pull_request --changed-files "$tmp_dir/vm_macos_sandbox_test"

write_paths release_meta Cargo.lock changelog.d/123.fixed.md
assert_plan false --platform windows --event push --changed-files "$tmp_dir/release_meta"
assert_plan false --platform macos --event push --changed-files "$tmp_dir/release_meta"

write_paths release_workflow .github/workflows/build-release-binaries.yml
write_policy_diff release_control.diff .github/workflows/build-release-binaries.yml '@@ -20,1 +20,1 @@
-      - run: scripts/release_runner_matrix.sh --mode primary
+      - run: scripts/release_runner_matrix.sh --mode policy'
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/release_workflow" --policy-diff "$tmp_dir/release_control.diff"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/release_workflow" --policy-diff "$tmp_dir/release_control.diff"

write_policy_diff release_windows.diff .github/workflows/build-release-binaries.yml '@@ -20,1 +20,1 @@
-          target: x86_64-pc-windows-msvc
+          target: x86_64-pc-windows-msvc
+          shell: pwsh'
assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/release_workflow" --policy-diff "$tmp_dir/release_windows.diff"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/release_workflow" --policy-diff "$tmp_dir/release_windows.diff"

write_policy_diff release_macos.diff .github/workflows/build-release-binaries.yml '@@ -20,1 +20,1 @@
-          target: aarch64-apple-darwin
+          target: aarch64-apple-darwin
+          runner: macos-latest'
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/release_workflow" --policy-diff "$tmp_dir/release_macos.diff"
assert_plan true --platform macos --event pull_request --changed-files "$tmp_dir/release_workflow" --policy-diff "$tmp_dir/release_macos.diff"

assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/release_workflow" --policy-diff "$tmp_dir/missing-policy.diff"

write_paths ci_only .github/workflows/ci.yml
write_diff package.diff '@@ -4,1 +4,1 @@
-      - run: make package-audit
+      - run: make package-audit-fast'
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/package.diff"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/package.diff"

write_diff windows.diff '@@ -7,1 +7,1 @@
-      - run: echo windows route
+      - run: echo windows route v2'
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/windows.diff"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/windows.diff"

write_diff windows_job.diff '@@ -10,1 +10,1 @@
-      - run: cargo test --target windows
+      - run: cargo test --target windows --locked'
assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/windows_job.diff"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/windows_job.diff"

write_diff macos.diff '@@ -13,1 +13,1 @@
-      - run: echo macos route
+      - run: echo macos route v2'
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/macos.diff"
assert_plan false --platform macos --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/macos.diff"

write_diff macos_job.diff '@@ -16,1 +16,1 @@
-      - run: cargo clippy
+      - run: cargo clippy --locked'
assert_plan false --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/macos_job.diff"
assert_plan true --platform macos --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/macos_job.diff"

assert_plan true --platform windows --event pull_request --changed-files "$tmp_dir/ci_only" --ci-diff "$tmp_dir/missing.diff"

echo "native_platform_ci_plan_test: ok"
