#!/usr/bin/env bash
# Regression test for release_ship.sh finalize tag-push idempotency.
#
# publish-release.yml can run after the release tag was already pushed by
# release_harn.harn. In that shape, finalize must skip the redundant tag push
# instead of invoking the local pre-push hook again.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
ship_script="$repo_root/scripts/release_ship.sh"

remote_tag_src=$(sed -n '/^remote_tag_commit() {/,/^}/p' "$ship_script")
push_tag_src=$(sed -n '/^push_tag_if_needed() {/,/^}/p' "$ship_script")
if [[ -z "$remote_tag_src" || -z "$push_tag_src" ]]; then
  echo "FAIL: tag-push helpers not found in $ship_script" >&2
  exit 1
fi

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

run_helper_case() {
  local mode="$1"
  local out="$tmp_root/$mode.out"
  local err="$tmp_root/$mode.err"
  local calls="$tmp_root/$mode.calls"
  : > "$calls"
  if (
    set -euo pipefail
    eval "$remote_tag_src"
    eval "$push_tag_src"
    git() {
      printf '%s\n' "$*" >> "$calls"
      case "$*" in
        "rev-parse HEAD")
          printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n'
          ;;
        "ls-remote --tags origin refs/tags/v1.2.3^{}")
          if [[ "$mode" == "remote-head" ]]; then
            printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/tags/v1.2.3^{}\n'
          elif [[ "$mode" == "remote-other" ]]; then
            printf 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\trefs/tags/v1.2.3^{}\n'
          fi
          ;;
        "ls-remote --tags origin refs/tags/v1.2.3")
          ;;
        "push origin v1.2.3")
          printf 'push-called\n' >> "$calls"
          ;;
        *)
          printf 'unexpected git invocation: %s\n' "$*" >&2
          return 2
          ;;
      esac
    }
    push_tag_if_needed v1.2.3
  ) >"$out" 2>"$err"; then
    printf '0\n'
  else
    printf '%s\n' "$?"
  fi
}

status="$(run_helper_case remote-head)"
if [[ "$status" != "0" ]]; then
  echo "FAIL: remote-head case failed with $status" >&2
  cat "$tmp_root/remote-head.err" >&2
  exit 1
fi
grep -q "Origin tag already exists at HEAD: v1.2.3" "$tmp_root/remote-head.out" \
  || { echo "FAIL: remote-head case did not report the no-op" >&2; exit 1; }
if grep -q "push-called" "$tmp_root/remote-head.calls"; then
  echo "FAIL: remote-head case still pushed the tag" >&2
  cat "$tmp_root/remote-head.calls" >&2
  exit 1
fi

status="$(run_helper_case missing)"
if [[ "$status" != "0" ]]; then
  echo "FAIL: missing-tag case failed with $status" >&2
  cat "$tmp_root/missing.err" >&2
  exit 1
fi
grep -q "push-called" "$tmp_root/missing.calls" \
  || { echo "FAIL: missing-tag case did not push" >&2; exit 1; }

status="$(run_helper_case remote-other)"
if [[ "$status" == "0" ]]; then
  echo "FAIL: remote-other case unexpectedly passed" >&2
  exit 1
fi
grep -q "already exists at bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" "$tmp_root/remote-other.out" \
  || { echo "FAIL: remote-other case did not report the conflicting tag" >&2; exit 1; }
if grep -q "push-called" "$tmp_root/remote-other.calls"; then
  echo "FAIL: remote-other case pushed after detecting conflict" >&2
  cat "$tmp_root/remote-other.calls" >&2
  exit 1
fi

echo "release_ship_tag_push_idempotent_test: ok"
