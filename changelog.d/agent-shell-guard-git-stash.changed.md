The agent shell guard refuses `git stash` forms that write the repository-wide stash list and
suggests a work-in-progress commit instead, because every linked worktree shares that list.
`git stash list` and `git stash show` stay allowed.
