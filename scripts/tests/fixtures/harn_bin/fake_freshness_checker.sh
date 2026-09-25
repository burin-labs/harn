#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
  record-evidence)
    printf 'harn-freshness-check-v4\nrepo-path=%064d\nchecker-build-id=aa\nchecker-content=%064d\nmanifest=%064d\n' 0 0 0
    ;;
  content-hash)
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "$2" | cut -d ' ' -f 1
    else
      shasum -a 256 "$2" | cut -d ' ' -f 1
    fi
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
