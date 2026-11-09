#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

origin="$tmp_root/origin.git"
work="$tmp_root/work"

git init -b main --bare --quiet "$origin"
git init -b main --quiet "$work"
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
    runs-on: ubuntu-24.04
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
    runs-on: ubuntu-24.04
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

# A shallow clone whose origin/main moved past the clone point shares no
# reachable merge base with HEAD. The base must still resolve, to the clone
# point, so the hook validates the local commits instead of exiting silently.
shallow="$tmp_root/shallow"
git clone --quiet --depth 1 --branch main "file://$origin" "$shallow"
git -C "$shallow" config user.email "test@example.com"
git -C "$shallow" config user.name "Test User"
git -C "$shallow" config commit.gpgsign false
git -C "$shallow" checkout --quiet -b shallow-feature
printf '%s\n' 'local change' > "$shallow/local.txt"
git -C "$shallow" add local.txt
git -C "$shallow" commit --quiet -m "local"
printf '%s\n' 'pub fn later() {}' >> "$work/crates/harn-vm/src/lib.rs"
git -C "$work" checkout --quiet main
git -C "$work" commit --quiet -am "later mainline"
git -C "$work" push --quiet origin main
git -C "$shallow" fetch --quiet --depth 1 origin +main:refs/remotes/origin/main
if git -C "$shallow" merge-base HEAD origin/main >/dev/null 2>&1; then
  echo "fixture no longer reproduces a shallow clone without a merge base" >&2
  exit 1
fi
shallow_files=$(
  cd "$shallow"
  # shellcheck source=/dev/null
  . "$repo_root/.githooks/lib.sh"
  hook_write_push_files "$tmp_root/shallow-files.txt" "$(hook_validation_base 2>/dev/null)"
  cat "$tmp_root/shallow-files.txt"
)
if [ "$shallow_files" != "local.txt" ]; then
  echo "a shallow clone should validate only its local commits, got: $shallow_files" >&2
  exit 1
fi

echo "pre_push_validation_range_test: ok"
