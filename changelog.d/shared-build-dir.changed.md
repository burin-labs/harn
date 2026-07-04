- **Dev setup now emits a machine-shared Cargo `build-dir`** (`$TMPDIR/cargo-build-shared`)
  alongside the per-worktree `target-dir`, so intermediate artifacts are deduped across
  worktrees (sccache's Rust hash is target-path-dependent and never was). Also fixed
  `scripts/prune_stale_targets.sh` dying silently under `set -euo pipefail` before it
  could GC stale per-worktree target dirs; it now always prints its summary line.
