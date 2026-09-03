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
# Two rules decide, in order:
#
#   1. Keep an entry a live worktree still maps to. "Live" means the worktree
#      directory exists on disk. A worktree deleted with `rm -rf` leaves its
#      git administrative record behind, and `git worktree list` keeps
#      reporting that record (as `prunable`) until someone runs
#      `git worktree prune`. Building the keep-set from the listing alone
#      therefore protected exactly the entries this GC exists to collect: a
#      cache entry outlived the worktree it caches, permanently.
#   2. Never remove a tree a live process owns. Liveness is decided by
#      process, not by mtime: a build paused between steps leaves its tree
#      untouched for a long time and reads as idle. Two signals count -- a
#      process holding cargo's advisory `.cargo-lock`, and a process naming
#      the entry on its command line. The probe excludes its own process
#      ancestry, because a GC whose own command line names the entry it is
#      scanning would otherwise manufacture a live owner for it.
#
# The idle-age bound survives both rules, as a staleness rule rather than a
# liveness one: it is the last line of defence for a worktree that discovery
# never reached, so a warm target outside HARN_TARGET_GC_ROOTS is still kept.
# When the process probe is unavailable (no usable `ps` or `lsof`), the GC says
# so rather than reading an unanswerable question as "idle". Removal is read
# back: the path must be gone afterwards, and every entry reported as kept must
# still be there.
#
# Cargo's default build scratch lives inside each per-worktree target directory,
# so the same liveness decision reclaims both. The removal loop re-validates
# that its root is one of the two managed families by exact name, and removes
# only a path it built from that root.
#
# Usage:
#   scripts/prune_stale_targets.sh [--dry-run]
# Env:
#   HARN_TARGET_GC_ROOTS        space-separated repo search roots
#                               (default: "$HOME/projects $HOME/.codex/worktrees /private/tmp")
#   HARN_TARGET_GC_FIND_DEPTH   max depth for nested worktree discovery (default 3)
#   HARN_TARGET_GC_MIN_AGE_SECS minimum idle age before removal (default 10800)
#   HARN_DEV_SETUP_STORAGE_ROOT one base for harn-target; when unset, sweep
#                               both the legacy $TMPDIR and durable cache roots
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/file_time.sh
source "$SCRIPT_DIR/lib/file_time.sh"

dry_run=0
[[ "${1:-}" == "--dry-run" ]] && dry_run=1

scanned=0; removed=0; kept=0; summary_printed=0
# Paths reported as kept, read back after a destructive pass.
kept_paths=()
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
  else
    roots=""
  fi
  # scanned is reported alongside the verdict so a zero here is readable as
  # "walked these roots and found nothing" rather than "walked nothing".
  echo "harn-target GC: scanned=$scanned kept=$kept removed=$removed (roots=$roots)$suffix"
}

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
  print_summary
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

# The pids this GC must never mistake for a build: its own process and every
# ancestor. An agent session or wrapper that names a cache entry on its command
# line is a common shape, and a probe that matches itself reports every entry
# it scans as live.
self_pids=" $$ "
collect_self_pids() {
  local pid parent guard=0
  pid="$$"
  while [ "$guard" -lt 32 ]; do
    parent="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d '[:space:]' || true)"
    case "$parent" in
      ''|*[!0-9]*) return 0 ;;
    esac
    [ "$parent" -le 1 ] && return 0
    self_pids="${self_pids}${parent} "
    pid="$parent"
    guard=$((guard + 1))
  done
}
collect_self_pids

is_self_pid() {
  case "$self_pids" in
    *" $1 "*) return 0 ;;
  esac
  return 1
}

# A `ps` that returns nothing has not proven the machine is idle, it has failed
# to answer. Establish once that the tool can produce a non-empty listing, and
# degrade loudly rather than reading its silence as "no owners".
probe_mode="process"
probe_note=""
if ! ps_probe="$(ps -eo pid=,args= 2>/dev/null)" || [ -z "$ps_probe" ]; then
  probe_mode="degraded"
  probe_note="no usable process listing"
elif ! command -v lsof >/dev/null 2>&1; then
  probe_mode="degraded"
  probe_note="lsof unavailable, cannot see build locks"
fi
if [ "$probe_mode" = "degraded" ]; then
  echo "harn-target GC: process liveness unavailable ($probe_note); the idle age of ${min_age}s is the only remaining guard"
fi

# Pids that own a cache entry, one per line. Cargo holds an advisory lock under
# the target dir for the whole build, so a paused build still holds it; a build
# configured on the command line names the path in its args.
entry_live_pids() {
  local dir="$1" lock pid line snapshot lsof_out
  for lock in "$dir"/.cargo-lock "$dir"/*/.cargo-lock; do
    [ -f "$lock" ] || continue
    lsof_out="$(lsof -t -- "$lock" 2>/dev/null || true)"
    while IFS= read -r pid; do
      case "$pid" in
        ''|*[!0-9]*) continue ;;
      esac
      is_self_pid "$pid" && continue
      printf '%s\n' "$pid"
    done <<< "$lsof_out"
  done

  # Snapshot per entry rather than once per run: a sweep of a large root takes
  # tens of seconds, and a build that starts during it still owns its tree.
  # Capture the whole listing and then match it. Piping `ps` into `grep -q`
  # would let grep exit on its first hit and SIGPIPE `ps`, which under
  # `pipefail` turns a present process into a failed read.
  snapshot="$(ps -eo pid=,args= 2>/dev/null || true)"
  [ -n "$snapshot" ] || return 0
  while IFS= read -r line; do
    case "$line" in
      ''|*[!0-9]*) continue ;;
    esac
    is_self_pid "$line" && continue
    printf '%s\n' "$line"
  done <<< "$(printf '%s\n' "$snapshot" | awk -v d="$dir" '
    {
      i = index($0, d)
      if (i == 0) next
      c = substr($0, i + length(d), 1)
      # Require a boundary so entry "foo" does not match sibling "foo-bar".
      if (c == "" || c == "/" || c == " " || c == "\t" || c == ":" || c == "\"" || c == "'"'"'") print $1
    }')"
}

# Build the keep-set: the basename of every harn-target dir that a live
# worktree still points at. Prefer the authoritative target-dir baked into the
# worktree's .cargo/config.toml; fall back to the derived <parent>-<leaf> name.
keep_file="$(mktemp)"
release_keep_file="$(mktemp)"
# Print the summary from the EXIT trap so no stray failure can ever make the
# GC die silently again.
trap 'rm -f "$keep_file" "$release_keep_file"; print_summary' EXIT

live_worktrees() {
  discover_repo_roots | while read -r repo; do
    [ -n "$repo" ] || continue
    git -C "$repo" worktree list --porcelain 2>/dev/null \
      | awk '/^worktree /{print substr($0,10)}' || true
  done | sort -u | while read -r wt; do
    [ -n "$wt" ] || continue
    # The record is not the worktree. `git worktree list` keeps reporting a
    # deleted worktree until someone prunes the metadata, and treating that
    # record as a live mapping is what kept orphaned caches alive.
    [ -d "$wt" ] || continue
    printf '%s\n' "$wt"
  done
}

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

live_worktrees | while read -r wt; do
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
  local d name m sz pids
  # Removal is destructive, so the root is re-validated here rather than only
  # where it was discovered. Both managed families are named exactly.
  case "$(basename "$target_root")" in
    harn-target|release-gate-target) ;;
    *)
      echo "refusing to prune: root '''$target_root''' is not a managed target root" >&2
      exit 1
      ;;
  esac
  walked_roots+=("$target_root")
  for d in "$target_root"/*; do
    [ -d "$d" ] || continue
    scanned=$((scanned + 1))
    name="$(basename "$d")"
    if grep -qxF "$name" "$keep"; then
      echo "keep (live worktree): $name"
      kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi

    # Liveness is decided by process. An age bound alone cannot answer it: a
    # build paused between steps leaves its tree untouched and reads as idle.
    if [ "$probe_mode" = "process" ]; then
      pids="$(entry_live_pids "$d" | sort -u | tr '\n' ' ')"
      pids="${pids% }"
      if [ -n "$pids" ]; then
        echo "keep (live process: $pids): $name"
        kept=$((kept + 1)); kept_paths+=("$d"); continue
      fi
    fi

    # The age bound stays, as a staleness rule rather than a liveness one. It
    # is the last line of defence when worktree discovery is incomplete -- a
    # root that is not in HARN_TARGET_GC_ROOTS has no keep-set entry, and a
    # warm target for such a worktree must still survive.
    if ! m="$(file_mtime_epoch "$d")"; then
      echo "keep (mtime unavailable): $name"; kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi
    if [ "$m" -ge "$cutoff" ]; then
      echo "keep (recently active): $name"
      kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi

    sz=$(du -sh "$d" 2>/dev/null | cut -f1 || true)
    if [ "$dry_run" -eq 1 ]; then
      echo "would remove orphan: $name (${sz:-?})"
      removed=$((removed + 1))
      continue
    fi

    # Remove one exact path this loop built from the validated root. Nothing
    # here expands a pattern, and nothing outside the root is reachable.
    if [ "$(dirname "$d")" != "$target_root" ] || [ "$name" = "." ] || [ "$name" = ".." ]; then
      echo "refusing to remove a path outside the managed root: $d" >&2
      kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi
    echo "removing orphan: $name (${sz:-?})"
    rm -rf "$d" || true
    # Removal is destructive, so read the decision back instead of trusting
    # the exit status of an `rm` that is deliberately failure-tolerant.
    if [ -e "$d" ]; then
      echo "warning: orphan survived removal, still present: $d" >&2
      kept=$((kept + 1))
      continue
    fi
    removed=$((removed + 1))
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
live_worktrees | while read -r wt; do
  [ -n "$wt" ] || continue
  # Mirrors release_gate.sh::release_gate_target_name.
  printf '%s\n' "$(printf '%s' "$(basename "$wt")" | tr -c 'A-Za-z0-9._-' '-')"
done | sort -u > "$release_keep_file" || true

if [ "${#release_target_roots[@]}" -gt 0 ]; then
  for release_root in "${release_target_roots[@]}"; do
    prune_root "$release_root" "$release_keep_file"
  done
fi

# Kept entries are read back too: a sweep that removed a neighbour must not
# have taken anything it reported keeping.
if [ "$dry_run" -eq 0 ] && [ "${#kept_paths[@]}" -gt 0 ]; then
  for kept_path in "${kept_paths[@]}"; do
    if [ ! -d "$kept_path" ]; then
      echo "error: entry reported as kept is gone: $kept_path" >&2
      exit 1
    fi
  done
fi

print_summary
