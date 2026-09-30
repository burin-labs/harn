#!/usr/bin/env bash
# Arm auto-merge on an automated release-lane pull request.
#
# The release pull request and the post-release development bump both merge
# through this call, under the release App token in GH_TOKEN. Arming only
# queues the merge: the pull request still needs every required check and
# review. Arm it when it opens, before its checks settle, because GitHub does
# not fire auto-merge for a pull request that was already clean when it was
# armed.
#
# A failed arm fails the caller. An unarmed release pull request looks open and
# healthy but never lands.
release_arm_auto_merge() {
  local pr_url="${1:-}"
  if [[ -z "$pr_url" ]]; then
    echo "error: release_arm_auto_merge requires a pull request URL" >&2
    return 1
  fi
  if ! gh pr merge "$pr_url" --auto --squash; then
    echo "error: could not arm auto-merge on $pr_url; it is open but will not merge on its own" >&2
    return 1
  fi
  echo "Armed auto-merge (squash) on $pr_url"
}
