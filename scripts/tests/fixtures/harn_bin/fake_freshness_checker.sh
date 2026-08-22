#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
  record-evidence)
    printf 'harn-freshness-check-v3\nrepo-path=%064d\nchecker-build-id=aa\nchecker-content=%064d\nchecker-path=%064d\nmanifest=%064d\n' 0 0 0 0
    ;;
  verify)
    exit 0
    ;;
  verify-worktree)
    [[ -r "$2.freshness" && -r "$2.freshness.manifest" ]] || exit 1
    ;;
  *)
    echo "unexpected fake freshness-checker invocation: $*" >&2
    exit 2
    ;;
esac
