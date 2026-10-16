#!/usr/bin/env bash
# Enroll a newly opened, validated release-lane PR in the repository's
# existing delivery controller. Existing PRs retain their owner's grant:
# scheduled openers must never undo an explicit Ship withdrawal.
release_enroll_ship() {
  local pr_url="${1:-}"
  if [[ -z "$pr_url" ]]; then
    echo "error: release_enroll_ship requires a pull request URL" >&2
    return 1
  fi
  if ! gh pr edit "$pr_url" --add-label ship; then
    echo "error: could not enroll Smart Ship on $pr_url; the opened PR needs attention" >&2
    return 1
  fi
  echo "Enrolled Smart Ship on $pr_url"
}
