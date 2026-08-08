#!/usr/bin/env bash
# `verify_release_metadata.harn` must fail when git cannot answer, not report
# a verified release.
#
# Every tag-state check in that script treated a failed git command as "no tag"
# and returned clean. That is not a hypothetical: the CI audit lanes ran with an
# `include.path` in `.git/config` pointing at a file the Harn sandbox denies, and
# git exits 128 for *every* invocation when it cannot read a config include. The
# gate was a no-op that still printed "verified release metadata for vX.Y.Z".
set -euo pipefail

cd "$(dirname "$0")/../.."

harn_bin="${HARN_BIN:-}"
if [ -z "$harn_bin" ]; then
  echo "release_metadata_git_failure_test: HARN_BIN not set; skipping" >&2
  exit 0
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

cat > "$tmp_dir/git" <<'STUB'
#!/bin/sh
echo "fatal: unable to access '/runner/_temp/git-credentials-x.config': Permission denied" >&2
exit 128
STUB
chmod +x "$tmp_dir/git"

set +e
output="$(PATH="$tmp_dir:$PATH" "$harn_bin" run --no-sandbox scripts/verify_release_metadata.harn 2>&1)"
status=$?
set -e

if [ "$status" -eq 0 ]; then
  echo "expected a non-zero exit when git cannot answer; got 0 with:" >&2
  printf '%s\n' "$output" | sed 's/^/  /' >&2
  exit 1
fi

case "$output" in
  *"cannot verify release tag state"*) ;;
  *)
    echo "expected the failure to say the tag state could not be verified; got:" >&2
    printf '%s\n' "$output" | sed 's/^/  /' >&2
    exit 1
    ;;
esac

case "$output" in
  *"Permission denied"*) ;;
  *)
    echo "expected git's own message to be carried through; got:" >&2
    printf '%s\n' "$output" | sed 's/^/  /' >&2
    exit 1
    ;;
esac

echo "release_metadata_git_failure_test: OK (an unusable git fails the gate)"
