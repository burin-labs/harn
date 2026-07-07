#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

origin="$tmp_root/origin.git"
work="$tmp_root/work"

git init --bare --quiet "$origin"
git init --quiet "$work"
git -C "$work" config user.email "test@example.com"
git -C "$work" config user.name "Test User"
git -C "$work" config commit.gpgsign false
git -C "$work" remote add origin "$origin"

mkdir -p "$work/.github/workflows" "$work/crates/harn-vm/src" "$work/conformance/tests"
cat > "$work/Makefile" <<'EOF'
all:
	@true
EOF
cat > "$work/.github/workflows/ci.yml" <<'EOF'
name: CI
on: [push]
jobs:
  noop:
    runs-on: ubuntu-latest
    steps:
      - run: "true"
EOF
printf '%s\n' 'pub fn base() {}' > "$work/crates/harn-vm/src/lib.rs"
printf '%s\n' 'fn main(harness: Harness) {}' > "$work/conformance/tests/base.harn"
git -C "$work" add .
git -C "$work" commit --quiet -m "base"
git -C "$work" branch -M main
git -C "$work" push --quiet -u origin main

git -C "$work" checkout --quiet -b feature
cat >> "$work/Makefile" <<'EOF'
lint-actions:
	@true
EOF
cat >> "$work/.github/workflows/ci.yml" <<'EOF'
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: "true"
EOF
git -C "$work" add Makefile .github/workflows/ci.yml
git -C "$work" commit --quiet -m "feature workflow"
git -C "$work" push --quiet -u origin feature

git -C "$work" checkout --quiet main
printf '%s\n' 'pub fn mainline_rust() {}' >> "$work/crates/harn-vm/src/lib.rs"
printf '%s\n' 'fn mainline_harn(harness: Harness) {}' > "$work/conformance/tests/mainline.harn"
git -C "$work" add crates/harn-vm/src/lib.rs conformance/tests/mainline.harn
git -C "$work" commit --quiet -m "mainline rust and harn"
git -C "$work" push --quiet origin main

git -C "$work" checkout --quiet feature
git -C "$work" fetch --quiet origin main
git -C "$work" rebase --quiet origin/main

(
  cd "$work"
  # shellcheck source=/dev/null
  . "$repo_root/.githooks/lib.sh"

  push_base=$(hook_push_base)
  validation_base=$(hook_validation_base)

  old_range_files="$tmp_root/old-range-files.txt"
  validation_files="$tmp_root/validation-files.txt"
  git diff --name-only --diff-filter=ACMR "$push_base"...HEAD > "$old_range_files"
  hook_write_push_files "$validation_files" "$validation_base"
)

if ! grep -Fxq "crates/harn-vm/src/lib.rs" "$tmp_root/old-range-files.txt"; then
  echo "fixture no longer reproduces the stale-upstream push range" >&2
  cat "$tmp_root/old-range-files.txt" >&2
  exit 1
fi
if ! grep -Fxq "conformance/tests/mainline.harn" "$tmp_root/old-range-files.txt"; then
  echo "fixture no longer includes mainline Harn changes in the push range" >&2
  cat "$tmp_root/old-range-files.txt" >&2
  exit 1
fi

cat > "$tmp_root/expected-validation-files.txt" <<'EOF'
.github/workflows/ci.yml
Makefile
EOF
sort "$tmp_root/validation-files.txt" > "$tmp_root/actual-validation-files.txt"
sort "$tmp_root/expected-validation-files.txt" > "$tmp_root/sorted-expected-validation-files.txt"

if ! diff -u "$tmp_root/sorted-expected-validation-files.txt" "$tmp_root/actual-validation-files.txt"; then
  echo "validation range should include only the PR branch delta against origin/main" >&2
  exit 1
fi

echo "pre_push_validation_range_test: ok"
