- The command risk scanner labels git commands that discard uncommitted work,
  untracked files, or stash entries (`git restore .`, `git checkout -- <path>`,
  `git switch --discard-changes`, `git clean -f`, `git stash drop`) with
  `git_discard_changes`, so a command policy can require approval for them.
