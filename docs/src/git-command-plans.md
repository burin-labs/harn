# Git command plans

`git_command_plan` from `std/git` returns `{operation, argv, mutating}` for a
structured `GitCommandRequest`. It performs no execution and grants no
permission. A host can inspect the plan, request approval, and pass `argv` to
its existing process runner. Never execute a joined or quoted rendering of
`argv` as a shell command.

Supported operations are `status`, `diff`, `log`, `branch`, `add`, `commit`,
`checkout`, `push`, `worktree_add`, `worktree_list`, `worktree_remove`, `open_pr`,
`pr_view`, `pr_checks`, `pr_merge`, `issue_view`, and `issue_create`. Forge
operations select `gh` by default or `glab` with `forge: "gitlab"`.

Arguments have closed types: booleans remain booleans, paths and labels are
lists of strings, and issue or pull-request numbers are positive integers.
Invalid arguments and unknown operations throw before a process starts.
Push plans accept ordinary branch names; force-push and remote-deletion
refspecs are refused. A bare `branch` inspects branches. Branch creation and
checkout mutate the repository. The returned `mutating` field describes the
operation; the host still owns permission policy and workspace confinement.

Branch creation uses `git checkout -b`, so it works with Git versions predating
`git switch`. Worktree listing requests `--porcelain -z` to preserve path
identity. `git_worktree_parse` decodes that output into `GitWorktreeEntry`
records and also accepts unquoted newline-delimited porcelain. Quoted legacy
paths require NUL-delimited output rather than guessing at their identity. Missing executables or
unsupported command options remain process failures; the plan never treats
an unavailable command as a successful observation.

GitLab checks address the current branch. Supplying a pull-request `number`
with `forge: "gitlab"` refuses the request instead of silently inspecting a
different target.
