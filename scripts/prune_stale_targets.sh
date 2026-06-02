#!/usr/bin/env bash
# Prune orphaned per-worktree Cargo target dirs.
#
# dev_setup.sh redirects each worktree's CARGO_TARGET_DIR to
# "$TMPDIR/harn-target/<parent>-<leaf>" so parallel worktree builds don't
# clobber each other. Nothing reclaimed those dirs when the worktree was
# removed, so they accumulated (observed: ~1 TB across dozens of deleted
# agent/codex worktrees). This script deletes any harn-target/* dir that no
# live git worktree still maps to.
#
# Safe by construction: it only removes a dir when (a) no current worktree of
# any repo under the search roots derives or references that dir, and (b) the
# dir has not been modified within HARN_TARGET_GC_MIN_AGE_SECS (default 3h),
# so an in-flight `make setup`/build is never touched.
#
# Usage:
#   scripts/prune_stale_targets.sh [--dry-run]
# Env:
#   HARN_TARGET_GC_ROOTS        space-separated repo search roots (default "$HOME/projects")
#   HARN_TARGET_GC_MIN_AGE_SECS minimum idle age before deletion (default 10800)
#   TMPDIR                      base for harn-target (default /tmp)
set -euo pipefail

dry_run=0
[[ "${1:-}" == "--dry-run" ]] && dry_run=1

target_root="${TMPDIR:-/tmp}/harn-target"
target_root="${target_root//\/\///}"   # collapse accidental double slash
[[ -d "$target_root" ]] || { echo "no harn-target dir at $target_root; nothing to prune"; exit 0; }

roots="${HARN_TARGET_GC_ROOTS:-$HOME/projects}"
min_age="${HARN_TARGET_GC_MIN_AGE_SECS:-10800}"
cutoff=$(( $(date +%s) - min_age ))

# Build the keep-set: the basename of every harn-target dir that a live
# worktree still points at. Prefer the authoritative target-dir baked into the
# worktree's .cargo/config.toml; fall back to the derived <parent>-<leaf> name.
keep_file="$(mktemp)"
trap 'rm -f "$keep_file"' EXIT
for root in $roots; do
  [ -d "$root" ] || continue
  for repo in "$root"/*; do
    [ -d "$repo/.git" ] || [ -f "$repo/.git" ] || continue
    git -C "$repo" worktree list --porcelain 2>/dev/null \
      | awk '/^worktree /{print substr($0,10)}'
  done
done | sort -u | while read -r wt; do
  [ -n "$wt" ] || continue
  cfg="$wt/.cargo/config.toml"
  if [ -f "$cfg" ]; then
    td=$(grep -E '^[[:space:]]*target-dir[[:space:]]*=' "$cfg" 2>/dev/null \
         | head -1 | sed -E 's/^[^"]*"//; s/".*$//' || true)
    if [ -n "$td" ]; then basename "$td"; fi
  fi
  # derived-name fallback (matches dev_setup.sh::derive_target_dir)
  printf '%s-%s\n' "$(basename "$(dirname "$wt")")" "$(basename "$wt")"
done | sort -u > "$keep_file"

removed=0; kept=0
for d in "$target_root"/*; do
  [ -d "$d" ] || continue
  name="$(basename "$d")"
  if grep -qxF "$name" "$keep_file"; then kept=$((kept+1)); continue; fi
  m=$(stat -f %m "$d" 2>/dev/null || echo 0)
  if [ "${m:-0}" -ge "$cutoff" ]; then
    echo "skip (recently active): $name"; kept=$((kept+1)); continue
  fi
  sz=$(du -sh "$d" 2>/dev/null | cut -f1 || true)
  if [ "$dry_run" -eq 1 ]; then
    echo "would remove orphan: $name (${sz:-?})"
  else
    echo "removing orphan: $name (${sz:-?})"
    rm -rf "$d" || true
  fi
  removed=$((removed+1))
done

echo "harn-target GC: kept=$kept ${dry_run:+would-}removed=$removed (root=$target_root)"
