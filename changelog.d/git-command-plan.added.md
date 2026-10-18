`std/git` can plan structured Git, GitHub CLI, and GitLab CLI operations as
typed argument lists. Hosts retain execution and approval authority without
rebuilding shell commands. Branch creation uses `git checkout -b`, which also
works on Git versions predating `git switch`. Worktree record decoding preserves
spaces and newlines in NUL-delimited paths.
