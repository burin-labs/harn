#!/usr/bin/env bash
# Prune orphaned per-worktree Cargo target dirs.
#
# dev_setup.sh redirects each worktree's CARGO_TARGET_DIR to
# "$HARN_DEV_SETUP_STORAGE_ROOT/harn-target/<parent>-<leaf>" so parallel
# worktree builds do not clobber each other. Nothing reclaimed those dirs when
# the worktree was
# removed, so they accumulated (observed: ~1 TB across dozens of deleted
# agent/codex worktrees). This script deletes any harn-target/* dir that no
# live git worktree still maps to.
#
# Safe by construction: it only removes a dir when (a) no current worktree of
# any repo under the search roots derives or references that dir, and (b) the
# dir has not been modified within HARN_TARGET_GC_MIN_AGE_SECS (default 3h),
# so an in-flight `make setup`/build is never touched.
#
# The machine-shared Cargo build-dir
# ("$HARN_DEV_SETUP_STORAGE_ROOT/cargo-build-shared", emitted by
# dev_setup.sh) is intentionally OUTSIDE the harn-target root and must never
# be pruned here: it is shared by every worktree, so no per-worktree liveness
# check applies. An assertion below pins the rm loop to a root whose basename
# is exactly "harn-target".
#
# Usage:
#   scripts/prune_stale_targets.sh [--dry-run]
# Env:
#   HARN_TARGET_GC_ROOTS        space-separated repo search roots
#                               (default: "$HOME/projects $HOME/.codex/worktrees /private/tmp")
#   HARN_TARGET_GC_FIND_DEPTH   max depth for nested worktree discovery (default 3)
#   HARN_TARGET_GC_MIN_AGE_SECS minimum idle age before deletion (default 10800)
#   HARN_DEV_SETUP_STORAGE_ROOT base for harn-target (default: $TMPDIR or the
#                               Rust-only setup cache root)
set -euo pipefail

dry_run=0
[[ "${1:-}" == "--dry-run" ]] && dry_run=1

storage_root="${HARN_DEV_SETUP_STORAGE_ROOT:-${TMPDIR:-/tmp}}"
target_root="${storage_root}/harn-target"
target_root="${target_root//\/\///}"   # collapse accidental double slash
[[ -d "$target_root" ]] || { echo "no harn-target dir at $target_root; nothing to prune"; exit 0; }

default_roots() {
  printf '%s\n' "$HOME/projects"
  printf '%s\n' "$HOME/.codex/worktrees"
  printf '%s\n' "/private/tmp"
}

if [[ -n "${HARN_TARGET_GC_ROOTS:-}" ]]; then
  roots="${HARN_TARGET_GC_ROOTS}"
else
  roots="$(default_roots | tr '\n' ' ')"
fi
find_depth="${HARN_TARGET_GC_FIND_DEPTH:-3}"
min_age="${HARN_TARGET_GC_MIN_AGE_SECS:-10800}"
cutoff=$(( $(date +%s) - min_age ))

# Every stage here must be failure-tolerant: `find` over roots like
# /private/tmp exits non-zero on permission-denied entries even with stderr
# suppressed, and under `set -euo pipefail` a single failing pipeline stage
# used to kill the whole script before it printed its summary (the GC was
# silently dead for months this way). Hence the `|| true` guards.
discover_repo_roots() {
  local root git_marker
  for root in $roots; do
    [ -d "$root" ] || continue
    if [ -d "$root/.git" ] || [ -f "$root/.git" ]; then
      printf '%s\n' "$root"
    fi
    find "$root" -maxdepth "$find_depth" \
      \( -name .git -type d -o -name .git -type f \) -print 2>/dev/null \
      | while IFS= read -r git_marker; do
          dirname "$git_marker" || true
        done || true
  done | sort -u || true
}

mtime_epoch() {
  stat -f %m "$1" 2>/dev/null || stat -c %Y "$1" 2>/dev/null || echo 0
}

# Refuse to sweep anything but a harn-target root. This keeps the rm loop
# away from siblings under the storage root — most importantly the
# machine-shared "cargo-build-shared" build-dir — even if target-root
# derivation changes.
if [ "$(basename "$target_root")" != "harn-target" ]; then
  echo "refusing to prune: root '$target_root' basename is not 'harn-target'" >&2
  exit 1
fi

removed=0; kept=0; summary_printed=0

print_summary() {
  [ "$summary_printed" -eq 1 ] && return 0
  summary_printed=1
  suffix=""
  [ "$dry_run" -eq 1 ] && suffix=" (dry-run)"
  echo "harn-target GC: kept=$kept removed=$removed (root=$target_root)$suffix"
}

# Build the keep-set: the basename of every harn-target dir that a live
# worktree still points at. Prefer the authoritative target-dir baked into the
# worktree's .cargo/config.toml; fall back to the derived <parent>-<leaf> name.
keep_file="$(mktemp)"
# Print the summary from the EXIT trap so no stray failure can ever make the
# GC die silently again.
trap 'rm -f "$keep_file"; print_summary' EXIT
discover_repo_roots | while read -r repo; do
  [ -n "$repo" ] || continue
  git -C "$repo" worktree list --porcelain 2>/dev/null \
    | awk '/^worktree /{print substr($0,10)}' || true
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
done | sort -u > "$keep_file" || true
for d in "$target_root"/*; do
  [ -d "$d" ] || continue
  name="$(basename "$d")"
  if grep -qxF "$name" "$keep_file"; then kept=$((kept+1)); continue; fi
  m="$(mtime_epoch "$d")"
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

print_summary
