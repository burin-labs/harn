- **Harn target-dir cleanup now recognizes nested Codex worktrees.** The
  `prune_stale_targets.sh` helper scans bounded nested repo roots such as
  `$HOME/.codex/worktrees` and uses portable mtimes, so per-worktree Cargo
  targets are kept or pruned based on the live worktree set instead of only
  direct children of `$HOME/projects`.
