- **Harn source discovery now skips generated and local cache directories.**
  `harn check`, `harn lint`, and `harn fmt` no longer recursively scan
  dependency caches, build outputs, local run artifacts, or worktree copies
  when a project directory is passed as the target.
