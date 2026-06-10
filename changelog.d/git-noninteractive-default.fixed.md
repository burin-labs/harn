- **Harn's git stdlib is now non-interactive by default, so a `.harn` git
  operation can no longer hang on a credential or host-key prompt in a
  TTY-less context (`harn serve`, `@job`, CI).** Every git subprocess the
  stdlib spawns — the receipt builtins (`git.fetch`/`git.push`/`git.rebase`/
  `git.worktree_create`/…) and the `std/worktree` helpers (`worktree_create`,
  `worktree_status`, `worktree_diff`, `worktree_remove`, `worktree_shell`) —
  now runs with `GIT_TERMINAL_PROMPT=0`, an empty `GIT_ASKPASS`/`SSH_ASKPASS`,
  and an `ssh -oBatchMode=yes` transport so git fails fast instead of blocking
  on an interactive prompt. The guard is merged (`env_mode: "merge"`), so
  inherited `PATH`/`HOME` and credentials supplied via env, `.netrc`,
  credential helpers, or a pre-loaded ssh-agent continue to authenticate
  push/clone/fetch exactly as before — only *interactive prompting* is
  disabled. `std/worktree` helpers take an optional trailing `options` dict, so
  a caller that genuinely wants interactive git can re-enable it by passing its
  own `env`/`env_mode`.
