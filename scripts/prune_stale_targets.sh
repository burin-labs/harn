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
# Cargo's default build scratch lives inside each per-worktree target directory,
# so the same liveness decision reclaims both. An assertion below pins the rm
# loop to a root whose basename is exactly "harn-target".
#
# Usage:
#   scripts/prune_stale_targets.sh [--dry-run]
# Env:
#   HARN_TARGET_GC_ROOTS        space-separated repo search roots
#                               (default: "$HOME/projects $HOME/.codex/worktrees /private/tmp")
#   HARN_TARGET_GC_FIND_DEPTH   max depth for nested worktree discovery (default 3)
#   HARN_TARGET_GC_MIN_AGE_SECS minimum idle age before deletion (default 10800)
#   HARN_DEV_SETUP_STORAGE_ROOT one base for harn-target; when unset, sweep
#                               both the legacy $TMPDIR and durable cache roots
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/file_time.sh
source "$SCRIPT_DIR/lib/file_time.sh"

dry_run=0
[[ "${1:-}" == "--dry-run" ]] && dry_run=1

storage_roots() {
  if [[ -n "${HARN_DEV_SETUP_STORAGE_ROOT:-}" ]]; then
    printf '%s\n' "${HARN_DEV_SETUP_STORAGE_ROOT}"
    return
  fi

  printf '%s\n' "${TMPDIR:-/tmp}"
  printf '%s/harn/dev-setup\n' "${XDG_CACHE_HOME:-$HOME/.cache}"
}

target_roots=()
while IFS= read -r storage_root; do
  target_root="${storage_root}/harn-target"
  target_root="${target_root//\/\///}"   # collapse accidental double slash
  [[ -d "$target_root" ]] || continue
  if [[ "$(basename "$target_root")" != "harn-target" ]]; then
    echo "refusing to prune: root '$target_root' basename is not 'harn-target'" >&2
    exit 1
  fi
  target_roots+=("$target_root")
done < <(storage_roots | awk '!seen[$0]++')

# The release gate's Cargo caches sit beside the setup targets under the same
# storage roots (#6212).
release_target_roots=()
while IFS= read -r storage_root; do
  release_root="${storage_root%/}/release-gate-target"
  [[ -d "$release_root" ]] || continue
  release_target_roots+=("$release_root")
done < <(storage_roots | awk '!seen[$0]++')

if [[ "${#target_roots[@]}" -eq 0 && "${#release_target_roots[@]}" -eq 0 ]]; then
  echo "no harn-target dirs at configured setup storage roots; nothing to prune"
  exit 0
fi

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

removed=0; kept=0; summary_printed=0
# Every root actually walked, so the summary cannot claim narrower coverage
# than the run had.
walked_roots=()

print_summary() {
  [ "$summary_printed" -eq 1 ] && return 0
  summary_printed=1
  suffix=""
  [ "$dry_run" -eq 1 ] && suffix=" (dry-run)"
  local roots
  if [ "${#walked_roots[@]}" -gt 0 ]; then
    roots="$(IFS=,; echo "${walked_roots[*]}")"
  elif [ "${#target_roots[@]}" -gt 0 ]; then
    roots="$(IFS=,; echo "${target_roots[*]}")"
  elif [ "${#release_target_roots[@]}" -gt 0 ]; then
    roots="$(IFS=,; echo "${release_target_roots[*]}")"
  else
    roots=""
  fi
  echo "harn-target GC: kept=$kept removed=$removed (roots=$roots)$suffix"
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
prune_root() {
  local target_root="$1"
  local keep="$2"
  local d name m sz
  walked_roots+=("$target_root")
  for d in "$target_root"/*; do
    [ -d "$d" ] || continue
    name="$(basename "$d")"
    if grep -qxF "$name" "$keep"; then kept=$((kept+1)); continue; fi
    if ! m="$(file_mtime_epoch "$d")"; then
      echo "skip (mtime unavailable): $name"; kept=$((kept+1)); continue
    fi
    if [ "$m" -ge "$cutoff" ]; then
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
}

# Bash 3.2 treats an empty `"${array[@]}"` expansion as unbound under
# `set -u`. The counts are safe on every supported Bash and make the two
# independently optional root families explicit.
if [ "${#target_roots[@]}" -gt 0 ]; then
  for target_root in "${target_roots[@]}"; do
    prune_root "$target_root" "$keep_file"
  done
fi

# The release gate keeps its Cargo cache beside the setup targets rather than
# under `$TMPDIR`, where the OS used to reap it a file at a time (#6212). Each
# release root gets its own, and release worktrees are ephemeral, so without a
# GC here every finished release would leave a multi-gigabyte cache behind
# forever. These are named after the release root alone, so they need their own
# keep-set: a bare worktree leaf must not also protect a `<parent>-<leaf>` entry
# under `harn-target`.
release_keep_file="$(mktemp)"
trap 'rm -f "$keep_file" "$release_keep_file"; print_summary' EXIT
discover_repo_roots | while read -r repo; do
  [ -n "$repo" ] || continue
  git -C "$repo" worktree list --porcelain 2>/dev/null \
    | awk '/^worktree /{print substr($0,10)}' || true
done | sort -u | while read -r wt; do
  [ -n "$wt" ] || continue
  # Mirrors release_gate.sh::release_gate_target_name.
  printf '%s\n' "$(printf '%s' "$(basename "$wt")" | tr -c 'A-Za-z0-9._-' '-')"
done | sort -u > "$release_keep_file" || true

if [ "${#release_target_roots[@]}" -gt 0 ]; then
  for release_root in "${release_target_roots[@]}"; do
    prune_root "$release_root" "$release_keep_file"
  done
fi

print_summary
