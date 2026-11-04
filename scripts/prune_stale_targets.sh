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
# Three rules decide, in order:
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
#      untouched for a long time and reads as idle. An open file or a command
#      line naming the entry both count. Command-line matching excludes the
#      probe's ancestry; open handles from any process still protect the tree.
#   3. Bound what rule 1 keeps. Dropping orphans bounds nothing on a machine
#      whose worktrees are all alive, because every one of them keeps its tree
#      forever, and the cache grows with the number of worktrees the machine
#      has ever had at once. Warm entries are ranked by recency within their
#      root; the newest HARN_TARGET_GC_KEEP_RECENT are the working set and are
#      kept whatever their age, and the rest are retired once nothing has
#      built them for HARN_TARGET_GC_MAX_IDLE_SECS. Rule 2 outranks this: an
#      entry a live process owns never reaches the ranking, and neither does
#      one whose mtime this run could not read.
#
# The idle-age bound survives all three, as a staleness rule rather than a
# liveness one: it is the last line of defence for a worktree that discovery
# never reached, so a warm target outside HARN_TARGET_GC_ROOTS is still kept.
# When the process probe is unavailable (no usable `ps` or `lsof`), the GC says
# so and keeps the entry instead of treating it as idle. Removal is read
# back: the path must be gone afterwards, and every entry reported as kept must
# still be there.
#
# Cargo's default build scratch lives inside each per-worktree target directory,
# so the same liveness decision reclaims both. The removal loop re-validates
# that its root is one of the two managed families by exact name, and removes
# only a path it built from that root.
#
# Usage:
#   scripts/prune_stale_targets.sh [--dry-run] [--measure-bytes]
#   scripts/prune_stale_targets.sh [--dry-run] --host-maintenance
#   scripts/prune_stale_targets.sh [--dry-run] --remove-entry NAME [--remove-entry NAME]...
#   scripts/prune_stale_targets.sh [--protect-entry NAME]... [other options]
#
# `--measure-bytes` makes the summary account for allocated bytes removed and
# retained. It walks target trees, so periodic host maintenance should use it;
# ordinary worktree setup leaves it off and explicitly reports unmeasured
# bytes. The summary names the running policy checksum and the last successful
# policy from a receipt beside the shared target cache.
#
# `--host-maintenance` is the policy for the scheduled and disk-pressure
# sweeps. It implies `--measure-bytes`, protects the 3 most recent warm trees
# under pressure, and sets the size ceiling to an eighth of the filesystem that
# holds the cache (64 GiB when that size cannot be read). The setup defaults
# below are for a developer's own worktree setup, where a cap of 10 is the
# working set. On a host with 10 or fewer entries that cap protected every one
# of them, so the idle bound never fired: a 460 GiB build host held 133 GB of
# trees nobody had built in up to 42 hours and ran out of disk. Explicit
# HARN_TARGET_GC_* values still win over both policies.
#
# `--remove-entry` names entries to retire regardless of their rank or age. It
# exists because rank and age answer "was this touched recently", not "is this
# still wanted": a lane whose pull request merged yesterday leaves a warm tree
# that every automatic rule protects. An operator who has established that an
# entry is finished had no way to say so through this tool, and the alternative
# was removing paths around it, which skips the liveness probe and the
# read-back. Named entries still go through both: a live process always wins.
# `--protect-entry` keeps each named entry ahead of removal, rank, age, and size
# rules. Repeat it to reserve several lanes; each NAME is one exact basename,
# including spaces, and protects that name in both managed cache families.
# Env:
#   HARN_TARGET_GC_ROOTS        space-separated repo search roots
#                               (default: "$HOME/projects $HOME/.codex/worktrees /private/tmp")
#   HARN_TARGET_GC_FIND_DEPTH   max depth for nested worktree discovery (default 3)
#   HARN_TARGET_GC_MIN_AGE_SECS minimum idle age before any automatic removal,
#                               including size enforcement (default 10800)
#   HARN_TARGET_GC_KEEP_RECENT  warm trees per root kept whatever their age
#                               (default 10; a negative value disables the cap)
#   HARN_TARGET_GC_MAX_IDLE_SECS idle age past which a warm tree outside the
#                               most-recent set is retired (default 259200)
#   HARN_TARGET_GC_MAX_BYTES    per-root size ceiling; once the entries this
#                               run kept exceed it, the coldest entries past
#                               the minimum idle age are retired until the root
#                               fits. 0 (the default) disables it. Off by
#                               default because enforcing it costs
#                               a full `du` of every kept entry, which is
#                               seconds to minutes on a large root and is not
#                               a price every dev-setup should pay; enable it
#                               on the periodic cleanup path.
#   HARN_TARGET_GC_MIN_FREE_BYTES free-space floor for the cache filesystem.
#                               Host maintenance defaults to 10% of its size;
#                               setup defaults to 0, which disables this bound.
#                               Pressure bypasses the 72-hour idle limit, while
#                               preserving minimum age, recent keys, and live owners.
#   HARN_DEV_SETUP_STORAGE_ROOT one base for harn-target; when unset, sweep
#                               both the legacy $TMPDIR and durable cache roots
#   HARN_TARGET_GC_PROTECT      the caller's own entry (path or name), kept
#                               whatever its age or rank
#   HARN_TARGET_GC_HANDLE_PROBE optional absolute executable supplied by the
#                               host. It receives the lexical and physical
#                               target paths and must print exactly busy, idle,
#                               or unknown. Failures and unknown retain targets.
#                               Without it, Linux reads /proc and Cargo locks
#                               and other systems use lsof.
#   HARN_TARGET_GC_PROC_ROOT    the Linux process table (default /proc), for tests
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/file_time.sh
source "$SCRIPT_DIR/lib/file_time.sh"

# When was this target tree last built in?
#
# Not `file_mtime_epoch` on the entry itself. A Cargo target directory's own
# mtime only moves when something is created or removed at its top level, and
# Cargo writes into `debug/` for the whole life of a tree, so the entry's mtime
# is effectively its creation time and never advances no matter how much is
# built. `cargo_target_seed.sh` then makes that reading actively wrong: a
# restored seed carries the seed's timestamps, so a tree created minutes ago
# reports an mtime weeks old.
#
# That is not a slow clock, it is a measurement that cannot answer the question
# being asked, and the retention cap below acts on it. Measured on a live fleet
# cache: eight of fourteen entries reported an entry mtime of 2026-08-09 while
# their `debug/` directories read 2026-09-04, and one of them was reaped as a
# "cold cache" while its worktree was actively building. `debug/` tracked real
# activity correctly in every one of those rows.
#
# Take the newest of the entry and its profile directories. Three stats, no
# walk of a multi-gigabyte tree.
target_entry_activity_epoch() {
  local entry="$1"
  local newest="" candidate="" sub
  for sub in "$entry" "$entry/debug" "$entry/release"; do
    [ -e "$sub" ] || continue
    candidate="$(file_mtime_epoch "$sub")" || continue
    if [ -z "$newest" ] || [ "$candidate" -gt "$newest" ]; then
      newest="$candidate"
    fi
  done
  [ -n "$newest" ] || return 1
  printf '%s\n' "$newest"
}

dry_run=0
measure_bytes=0
host_maintenance=0
requested_entries=()
protected_entries=()
usage() {
  cat <<'EOF'
Usage: scripts/prune_stale_targets.sh [options]

  --dry-run            Report candidates without removing entries.
  --measure-bytes      Measure reclaimed and retained allocated bytes.
  --host-maintenance   Apply retention, size and free-space bounds; measure bytes.
  --remove-entry NAME  Retire this exact basename regardless of rank or age.
  --protect-entry NAME Keep this exact basename ahead of every removal rule.
  --help               Show this help without scanning or changing caches.

Repeat --remove-entry and --protect-entry to name several entries. Names cannot
contain slashes; names with spaces must be quoted. Protection and live processes
outrank named removal. Both managed cache families use the same protections.
HARN_TARGET_GC_PROTECT adds the caller's own target path or basename to the set.
Protected bytes still count toward the ceiling; an oversized retained root is
reported as partial. See the script's Env reference for the remaining bounds.
EOF
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    --dry-run) dry_run=1; shift ;;
    --measure-bytes) measure_bytes=1; shift ;;
    --host-maintenance) host_maintenance=1; measure_bytes=1; shift ;;
    --remove-entry|--protect-entry)
      [ "$#" -ge 2 ] || { echo "$1 needs a NAME" >&2; exit 2; }
      # One path segment only. A name carrying a separator could otherwise
      # address a directory outside the managed root.
      case "$2" in
        ""|.|..|*/*) echo "invalid $1 name: $2" >&2; exit 2 ;;
      esac
      if [ "$1" = --remove-entry ]; then
        requested_entries+=("$2")
      else
        protected_entries+=("$2")
      fi
      shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Was this entry named on the command line?
entry_requested() {
  local name="$1" want
  [ "${#requested_entries[@]}" -gt 0 ] || return 1
  for want in "${requested_entries[@]}"; do
    [ "$want" = "$name" ] && return 0
  done
  return 1
}

entry_protected() {
  local name="$1" want
  [ "${#protected_entries[@]}" -gt 0 ] || return 1
  for want in "${protected_entries[@]}"; do
    [ "$want" = "$name" ] && return 0
  done
  return 1
}

report_pending_named_entry() {
  local name="$1"
  if entry_requested "$name"; then
    blocked_named_entries=$((blocked_named_entries + 1))
    echo "pending named removal: $name"
  fi
  return 0
}

# Names the caller asked for that no root turned out to hold. Reported at the
# end: a removal request that silently matched nothing would read as success.
matched_entries=()

gc_policy_version="harn-target-gc/v2-$(cksum "$SCRIPT_DIR/prune_stale_targets.sh" | awk '{print $1 "-" $2}')"
scanned=0; removed=0; kept=0; pending_candidates=0; blocked_named_entries=0; ceiling_unproven_roots=0; pressure_unproven_roots=0; summary_printed=0
reclaimed_bytes=0; retained_bytes=0; unmeasured_entries=0
completed=0
receipt_root="${HARN_DEV_SETUP_STORAGE_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/harn/dev-setup}"
receipt_file="${receipt_root}/prune-stale-targets.last-success"
last_success="never"
last_success_policy="none"
if [ -f "$receipt_file" ]; then
  IFS= read -r last_success < "$receipt_file" || true
  [ -n "$last_success" ] || last_success="never"
  last_success_policy="$(sed -n '2p' "$receipt_file" 2>/dev/null || true)"
  [ -n "$last_success_policy" ] || last_success_policy="unknown"
fi
# Paths reported as kept, read back after a destructive pass.
kept_paths=()
# Every root actually walked, so the summary cannot claim narrower coverage
# than the run had.
walked_roots=()

# A name that matched nothing is reported by name. Without this, a typo or an
# already-removed entry would leave the run looking like it did the work.
report_unmatched_entries() {
  [ "${#requested_entries[@]}" -gt 0 ] || return 0
  local want found
  for want in "${requested_entries[@]}"; do
    found=0
    if [ "${#matched_entries[@]}" -gt 0 ]; then
      for name in "${matched_entries[@]}"; do
        [ "$name" = "$want" ] && { found=1; break; }
      done
    fi
    if [ "$found" -eq 0 ]; then
      echo "warning: --remove-entry matched no entry in any managed root: $want" >&2
    fi
  done
  # An `&&` as the last statement would return 1 whenever the name DID match,
  # and `set -e` turns that into a failing run that just did its job correctly.
  return 0
}

print_summary() {
  [ "$summary_printed" -eq 1 ] && return 0
  summary_printed=1
  report_unmatched_entries
  suffix=""
  [ "$dry_run" -eq 1 ] && suffix=" (dry-run)"
  local roots
  if [ "${#walked_roots[@]}" -gt 0 ]; then
    roots="$(IFS=,; echo "${walked_roots[*]}")"
  else
    roots=""
  fi
  local status="incomplete" reported_reclaimed="$reclaimed_bytes" reported_retained="$retained_bytes"
  if [ "$completed" -eq 1 ]; then
    status="complete"
    if [ "$dry_run" -eq 1 ]; then
      status="dry-run"
    elif [ "$ceiling_unproven_roots" -gt 0 ] || [ "$pressure_unproven_roots" -gt 0 ] || [ "$blocked_named_entries" -gt 0 ]; then
      status="partial"
    fi
  fi
  if [ "$measure_bytes" -eq 0 ]; then
    reported_reclaimed="unmeasured"
    reported_retained="unmeasured"
    unmeasured_entries="$scanned"
  fi
  local reported_pending=$((pending_candidates + blocked_named_entries))
  if [ "$dry_run" -eq 1 ]; then
    reported_pending=$((reported_pending + removed))
  fi
  # scanned is reported alongside the verdict so a zero here is readable as
  # "walked these roots and found nothing" rather than "walked nothing".
  echo "harn-target GC: policy=$gc_policy_version status=$status last_success=$last_success last_success_policy=$last_success_policy scanned=$scanned kept=$kept removed=$removed reclaimed_bytes=$reported_reclaimed retained_bytes=$reported_retained pending_candidates=$reported_pending unmeasured_entries=$unmeasured_entries ceiling_unproven_roots=$ceiling_unproven_roots pressure_unproven_roots=$pressure_unproven_roots blocked_named_entries=$blocked_named_entries (roots=$roots)$suffix"
}

record_success() {
  [ "$dry_run" -eq 0 ] || return 0
  [ "$pending_candidates" -eq 0 ] || return 1
  # A protected/live name is a measured policy refusal, not a failed deletion.
  # Report a partial pass without advancing the last-success receipt.
  [ "$blocked_named_entries" -eq 0 ] || return 0
  [ "$ceiling_unproven_roots" -eq 0 ] && [ "$pressure_unproven_roots" -eq 0 ] || return 0
  mkdir -p "$receipt_root"
  local receipt_tmp="${receipt_file}.$$" new_success
  new_success="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '%s\n%s\n' "$new_success" "$gc_policy_version" > "$receipt_tmp"
  if ! mv "$receipt_tmp" "$receipt_file"; then
    rm -f "$receipt_tmp"
    return 1
  fi
  last_success="$new_success"
  last_success_policy="$gc_policy_version"
}

storage_roots() {
  if [[ -n "${HARN_DEV_SETUP_STORAGE_ROOT:-}" ]]; then
    printf '%s\n' "${HARN_DEV_SETUP_STORAGE_ROOT}"
    return
  fi

  printf '%s\n' "${TMPDIR:-/tmp}"
  printf '%s/harn/dev-setup\n' "${XDG_CACHE_HOME:-$HOME/.cache}"
}

# Return measured total and available KiB; an empty or failed read is unknown.
filesystem_space() {
  local report total available
  report="$(df -Pk "$1" 2>/dev/null)" || return 1
  read -r total available <<< "$(printf '%s\n' "$report" | awk 'NR == 2 { print $2, $4 }')"
  case "$total" in ''|*[!0-9]*|0) return 1 ;; esac
  case "$available" in ''|*[!0-9]*) return 1 ;; esac
  [ "$available" -le "$total" ] || return 1
  printf '%s %s\n' "$total" "$available"
}

pressure_floor_bytes() {
  if [ "$host_maintenance" -eq 1 ] && [ -z "${HARN_TARGET_GC_MIN_FREE_BYTES:-}" ]; then
    printf '%s\n' "$(( $1 * 1024 / 10 ))"
  else
    printf '%s\n' "$min_free_bytes"
  fi
}

# One run at a time per host. The daily job and a disk-pressure trigger can
# start minutes apart, and two sweeps over one root each read back the other's
# removals as failures. The lock is a directory beside the success receipt
# (mkdir is atomic), holding the owner's pid and start time. A run that finds a
# live holder reports `status=skipped` and exits 0: nothing went wrong, another
# run is doing this work. A proven-dead holder or an old unrecorded lock can
# be replaced; age alone never overrides a live or unmeasured holder.
run_lock="${receipt_root}/prune-stale-targets.lock"
run_lock_held=0
release_run_lock() {
  [ "$run_lock_held" -eq 1 ] || return 0
  rm -f "$run_lock/owner"
  rmdir "$run_lock" 2>/dev/null || true
  run_lock_held=0
}
acquire_run_lock() {
  local stale_secs="${HARN_TARGET_GC_LOCK_STALE_SECS:-21600}" holder_pid holder_epoch now attempt
  local holder_state ps_self ps_holder ps_status
  case "$stale_secs" in
    ''|*[!0-9]*) echo "HARN_TARGET_GC_LOCK_STALE_SECS must be a non-negative integer: $stale_secs" >&2; exit 2 ;;
  esac
  mkdir -p "$receipt_root"
  for attempt in 1 2; do
    if mkdir "$run_lock" 2>/dev/null; then
      printf '%s %s\n' "$$" "$(date +%s)" > "$run_lock/owner"
      run_lock_held=1
      trap release_run_lock EXIT
      return 0
    fi
    holder_pid=""; holder_epoch=""
    if [ -f "$run_lock/owner" ]; then
      read -r holder_pid holder_epoch < "$run_lock/owner" || true
    fi
    # A lock whose owner file is not written yet belongs to a run that is
    # starting this instant; age it by the directory itself.
    case "$holder_epoch" in
      ''|*[!0-9]*) holder_epoch="$(file_mtime_epoch "$run_lock" 2>/dev/null || date +%s)" ;;
    esac
    now="$(date +%s)"
    holder_state=unrecorded
    case "$holder_pid" in
      ''|0|*[!0-9]*) ;;
      *)
        holder_state=unknown
        if kill -0 "$holder_pid" 2>/dev/null; then
          holder_state=live
        elif ps_self="$(ps -p "$$" -o pid= 2>&1)" &&
             [ "${ps_self//[[:space:]]/}" = "$$" ]; then
          if ps_holder="$(ps -p "$holder_pid" -o pid= 2>&1)"; then
            [ "${ps_holder//[[:space:]]/}" != "$holder_pid" ] || holder_state=live
          else
            ps_status=$?
            if [ "$ps_status" -eq 1 ] && [ -z "$ps_holder" ]; then
              holder_state=dead
            fi
          fi
        fi
        ;;
    esac
    if [ "$attempt" -eq 1 ] && {
      [ "$holder_state" = dead ] ||
        { [ "$holder_state" = unrecorded ] && [ $((now - holder_epoch)) -gt "$stale_secs" ]; }; }; then
      echo "harn-target GC: replacing stale run lock (holder_pid=${holder_pid:-unknown} age_secs=$((now - holder_epoch)))"
      rm -f "$run_lock/owner"
      rmdir "$run_lock" 2>/dev/null || true
      continue
    fi
    echo "harn-target GC: policy=$gc_policy_version status=skipped reason=another-run-active holder_pid=${holder_pid:-unknown} age_secs=$((now - holder_epoch)) scanned=0 kept=0 removed=0 reclaimed_bytes=0 (lock=$run_lock)"
    exit 0
  done
}
acquire_run_lock

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
  if ! record_success; then
    print_summary
    exit 1
  fi
  completed=1
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
# The retention cap. Dropping orphans bounds nothing on a machine whose
# worktrees are all alive: every live worktree keeps its tree forever, so the
# cache still grows with the number of worktrees the machine has ever had at
# once. The newest few are the working set and are never touched. Past that,
# a tree nobody has built in days is cache, and the cost of dropping it is a
# rebuild.
# Count and age answer "was this touched recently", never "is this small
# enough". Without a size rule a root stays healthy-looking at any size: a
# fleet that touches every entry inside the idle bound keeps all of them, and
# the count cap alone protects the newest ten however large they are.
default_max_bytes=0
default_min_free_bytes=0
default_keep_recent=10
max_bytes_source=default
if [ "$host_maintenance" -eq 1 ]; then
  default_keep_recent=3
  # An eighth of the filesystem holding the cache, so a small build host and a
  # large workstation each keep a share they can afford. A size that cannot be
  # read falls back to 64 GiB, about an eighth of the fleet's 460 GiB hosts.
  default_max_bytes=68719476736
  default_min_free_bytes=107374182400
  max_bytes_source=fallback
  policy_root="${target_roots[0]:-${release_target_roots[0]:-}}"
  if space="$(filesystem_space "$policy_root")"; then
    fs_kib="${space%% *}"
    default_max_bytes=$((fs_kib * 1024 / 8)); max_bytes_source=disk
    default_min_free_bytes=$((fs_kib * 1024 / 10))
  fi
fi
[ -n "${HARN_TARGET_GC_MAX_BYTES:-}" ] && max_bytes_source="env"
max_bytes="${HARN_TARGET_GC_MAX_BYTES:-$default_max_bytes}"
case "$max_bytes" in
  ''|*[!0-9]*) echo "HARN_TARGET_GC_MAX_BYTES must be a non-negative integer: $max_bytes" >&2; exit 2 ;;
esac
min_free_bytes="${HARN_TARGET_GC_MIN_FREE_BYTES:-$default_min_free_bytes}"
case "$min_free_bytes" in
  ''|*[!0-9]*) echo "HARN_TARGET_GC_MIN_FREE_BYTES must be a non-negative integer: $min_free_bytes" >&2; exit 2 ;;
esac
keep_recent="${HARN_TARGET_GC_KEEP_RECENT:-$default_keep_recent}"
max_idle="${HARN_TARGET_GC_MAX_IDLE_SECS:-259200}"
idle_cutoff=$(( $(date +%s) - max_idle ))
if [ "$host_maintenance" -eq 1 ]; then
  echo "harn-target GC host policy: keep_recent=$keep_recent max_idle_secs=$max_idle max_bytes=$max_bytes max_bytes_source=$max_bytes_source"
fi

# The entry belonging to whoever launched this sweep, which must survive it
# whatever the ranking says. Setup restores a Cargo target seed and then sweeps,
# and a restored seed carries the seed's timestamps at every depth, so the tree
# setup just installed can read as idle for weeks the moment it is created. The
# ranking would then retire the one entry the caller is about to build into.
#
# Comparison is by basename, so a caller that knows only its target-dir path
# still protects the right entry, and a name shared across the two managed roots
# protects both. Over-protecting keeps a tree that a later sweep will collect;
# under-protecting deletes a tree that is in use.
if [ -n "${HARN_TARGET_GC_PROTECT:-}" ]; then
  protected_entries+=("$(basename "${HARN_TARGET_GC_PROTECT}")")
fi

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
proc_root="${HARN_TARGET_GC_PROC_ROOT:-/proc}"
if ! ps_probe="$(ps -eo pid=,args= 2>/dev/null)" || [ -z "$ps_probe" ]; then
  probe_mode="degraded"
  probe_note="no usable process listing"
elif [ -n "${HARN_TARGET_GC_HANDLE_PROBE:-}" ] &&
     { [[ "$HARN_TARGET_GC_HANDLE_PROBE" != /* ]] || [ ! -x "$HARN_TARGET_GC_HANDLE_PROBE" ]; }; then
  probe_mode="degraded"
  probe_note="configured handle probe unavailable"
elif [ -z "${HARN_TARGET_GC_HANDLE_PROBE:-}" ] && [ "$(uname -s)" = Linux ]; then
  if ! command -v python3 >/dev/null 2>&1 || ! command -v flock >/dev/null 2>&1 ||
     [ ! -r "$proc_root/self/status" ]; then
    probe_mode="degraded"
    probe_note="python3, flock or $proc_root unavailable, cannot see build handles"
  fi
elif [ -z "${HARN_TARGET_GC_HANDLE_PROBE:-}" ] && ! command -v lsof >/dev/null 2>&1; then
  probe_mode="degraded"
  probe_note="lsof unavailable, cannot see build locks"
fi
if [ "$probe_mode" = "degraded" ]; then
  echo "harn-target GC: process liveness unavailable ($probe_note); targets will be kept"
fi

# Linux handle census. An unprivileged `lsof` cannot read root's processes,
# and every Linux host runs some, so a census that needed every process read
# never completed and retained every entry. This one reads /proc directly:
#
# - A process this user can inspect holds the entry when its working
#   directory, root, executable, an open descriptor or a mapping lies inside.
# - A process it cannot inspect that runs as another non-root user is out of
#   scope only when the entry's ancestor permissions deny that user search
#   access. Otherwise the census is unmeasured and the entry stays.
# - A privileged process stays unreadable: root, a process the kernel marks
#   non-dumpable such as `(sd-pam)`, or one holding capabilities this census
#   lacks such as `systemd --user`. A privileged Cargo build
#   still holds the entry's `.cargo-lock`, which any user can test, and a
#   command line naming the entry is matched with every other. A privileged
#   process reading the entry outside Cargo is the accepted blind spot for a
#   rebuildable cache.
linux_entry_handle_pids() {
  local dir="$1" physical="$2" lock lock_status locks
  python3 - "$proc_root" "$dir" "$physical" <<'PY' || return 1
import os
import stat
import sys

proc, raw, physical = sys.argv[1:4]
targets = {raw.rstrip("/") or "/", physical.rstrip("/") or "/"}


def inside(path):
    if path.endswith(" (deleted)"):
        path = path[: -len(" (deleted)")]
    return any(path == t or path.startswith(t + "/") for t in targets)


def reachable(uid, gids, caps):
    # CAP_DAC_OVERRIDE or CAP_DAC_READ_SEARCH bypasses search permission.
    if caps & 0b110:
        return True
    current = "/"
    for part in [p for p in physical.split("/") if p]:
        # Mode bits cannot rule out a user an access ACL admits.
        try:
            if "system.posix_acl_access" in os.listxattr(current):
                return True
        except OSError:
            pass
        mode = os.stat(current)
        if uid == mode.st_uid:
            allowed = mode.st_mode & stat.S_IXUSR
        elif mode.st_gid in gids:
            allowed = mode.st_mode & stat.S_IXGRP
        else:
            allowed = mode.st_mode & stat.S_IXOTH
        if not allowed:
            return False
        current = os.path.join(current, part)
    return True


def effective_caps(path):
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if line.startswith("CapEff:"):
                return int(line.split()[1], 16)
    return 0


own_caps = effective_caps(os.path.join(proc, "self", "status"))
holders = []
# A process table that hides other users' processes (hidepid) lists no init.
if not os.path.isdir(os.path.join(proc, "1")):
    sys.exit("unmeasured: the process table does not show process 1")
for name in os.listdir(proc):
    if not name.isdigit():
        continue
    base = os.path.join(proc, name)
    try:
        with open(os.path.join(base, "status"), encoding="utf-8", errors="replace") as handle:
            fields = dict(line.rstrip("\n").split(":\t", 1) for line in handle if ":\t" in line)
    except (FileNotFoundError, ProcessLookupError):
        continue
    try:
        uid = int(fields["Uid"].split()[1])
        gids = {int(fields["Gid"].split()[1])} | {int(g) for g in fields.get("Groups", "").split()}
        caps = int(fields.get("CapEff", "0"), 16)
    except (KeyError, IndexError, ValueError):
        sys.exit(f"unmeasured: process {name} has an unreadable status")
    paths = []
    try:
        for link in ("cwd", "root", "exe"):
            try:
                paths.append(os.readlink(os.path.join(base, link)))
            except FileNotFoundError:
                pass
        for fd in os.listdir(os.path.join(base, "fd")):
            try:
                paths.append(os.readlink(os.path.join(base, "fd", fd)))
            except FileNotFoundError:
                pass
        with open(os.path.join(base, "maps"), encoding="utf-8", errors="replace") as handle:
            for line in handle:
                columns = line.split(None, 5)
                if len(columns) == 6:
                    paths.append(columns[5].strip())
    except (FileNotFoundError, ProcessLookupError):
        continue
    except PermissionError:
        if any(inside(path) for path in paths):
            holders.append(name)
            continue
        try:
            # The kernel hands a non-dumpable process's files to root.
            protected = os.stat(os.path.join(base, "fd")).st_uid == 0
        except (FileNotFoundError, ProcessLookupError):
            continue
        privileged = uid == 0 or protected or caps & ~own_caps
        if privileged or not reachable(uid, gids, caps):
            continue
        sys.exit(f"unmeasured: process {name} (uid {uid}) can reach the entry but cannot be inspected")
    if any(inside(path) for path in paths):
        holders.append(name)
for holder in holders:
    print(holder)
PY
  # flock exits 1 only when another holder has the lock; any other failure
  # leaves the lock unmeasured, as does a lock search that cannot read the tree.
  locks="$(find "$physical" -maxdepth 3 -name .cargo-lock -type f 2>/dev/null)" || return 1
  while IFS= read -r lock; do
    [ -n "$lock" ] || continue
    lock_status=0
    flock -n -x "$lock" true 2>/dev/null || lock_status=$?
    case "$lock_status" in
      0) ;;
      1) printf '%s\n' cargo-lock ;;
      *) return 1 ;;
    esac
  done <<< "$locks"
}

# Pids that own a cache entry, one per line.
entry_live_pids() {
  local dir="$1" pid line snapshot lsof_out handle_pids physical handle_verdict
  physical="$(cd "$dir" && pwd -P)" || return 1
  # A reader can hold any artifact without naming the path in its arguments or
  # holding Cargo's lock. A failed or empty census cannot establish idleness.
  if [ -n "${HARN_TARGET_GC_HANDLE_PROBE:-}" ]; then
    handle_verdict="$("$HARN_TARGET_GC_HANDLE_PROBE" "$dir" "$physical" 2>/dev/null)" || return 1
    case "$handle_verdict" in
      busy) printf '%s\n' host-handle-probe ;;
      idle) ;;
      *) return 1 ;;
    esac
  elif [ "$(uname -s)" = Linux ]; then
    linux_entry_handle_pids "$dir" "$physical" || return 1
  else
    lsof_out="$(lsof -n -Fpfn 2>&1)" || return 1
    [ -n "$lsof_out" ] || return 1
    handle_pids="$(printf '%s\n' "$lsof_out" | HARN_GC_PATH_RAW="$dir" HARN_GC_PATH_PHYSICAL="$physical" awk '
    /^p[0-9]+$/ { pid = substr($0, 2); next }
    /^fNOFD$/ { incomplete = 1; next }
    /^f/ { next }
    /^n/ {
      path = substr($0, 2)
      if (path ~ /\((readlink|opendir|stat): (Permission denied|Operation not permitted)\)$/) incomplete = 1
      d = ENVIRON["HARN_GC_PATH_RAW"]
      p = ENVIRON["HARN_GC_PATH_PHYSICAL"]
      if (path == d || index(path, d "/") == 1 ||
          path == p || index(path, p "/") == 1) print pid
      next
    }
    { incomplete = 1 }
    END { if (incomplete) exit 1 }
    ')" || return 1
    while IFS= read -r pid; do
      case "$pid" in
        ''|*[!0-9]*) continue ;;
      esac
      printf '%s\n' "$pid"
    done <<< "$handle_pids"
  fi
  # Snapshot per entry rather than once per run: a sweep of a large root takes
  # tens of seconds, and a build that starts during it still owns its tree.
  # Capture the whole listing and then match it. Piping `ps` into `grep -q`
  # would let grep exit on its first hit and SIGPIPE `ps`, which under
  # `pipefail` turns a present process into a failed read.
  snapshot="$(ps -eo pid=,args= 2>/dev/null)" || return 1
  [ -n "$snapshot" ] || return 1
  while IFS= read -r line; do
    case "$line" in
      ''|*[!0-9]*) continue ;;
    esac
    is_self_pid "$line" && continue
    printf '%s\n' "$line"
  done <<< "$(printf '%s\n' "$snapshot" | awk -v raw="$dir" -v physical="$physical" '
    {
      # A command line can name the entry by its lexical or resolved path.
      paths[1] = raw; paths[2] = physical
      for (n = 1; n <= 2; n++) {
        d = paths[n]
        i = index($0, d)
        if (i == 0) continue
        c = substr($0, i + length(d), 1)
        # Require a boundary so entry "foo" does not match sibling "foo-bar".
        if (c == "" || c == "/" || c == " " || c == "\t" || c == ":" || c == "\"" || c == "'"'"'") { print $1; next }
      }
    }')"
}

# Build the keep-set: the basename of every harn-target dir that a live
# worktree still points at. Prefer the authoritative target-dir baked into the
# worktree's .cargo/config.toml; fall back to the derived <parent>-<leaf> name.
keep_file="$(mktemp)"
release_keep_file="$(mktemp)"
# Warm entries a live worktree still points at, one `<mtime> <path>` line per
# root, so the retention cap below can rank them by recency.
warm_file="$(mktemp)"
# Entries this run kept whose ONLY protection is rank or age. The size pass may
# consider entries from here and nowhere else, then applies the same minimum
# idle age as the orphan pass before evicting one. A live process, recent
# activity, or the caller's own entry wins no matter how far over the ceiling
# the root sits.
evictable_file="$(mktemp)"
pressure_recent_paths=()

pressure_protected() {
  local recent
  for recent in ${pressure_recent_paths[@]+"${pressure_recent_paths[@]}"}; do
    [ "$recent" = "$1" ] && return 0
  done
  return 1
}
# Print the summary from the EXIT trap so no stray failure can ever make the
# GC die silently again.
trap 'rm -f "$keep_file" "$release_keep_file" "$warm_file" "$evictable_file"; print_summary; release_run_lock' EXIT

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
  local d name m pids
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
  # Warm entries are ranked across this root, so the list starts empty for it.
  : > "$warm_file"
  # Same for the size pass: its candidates are per-root, and carrying them
  # between roots would let one root's ceiling evict another root's entries.
  : > "$evictable_file"
  pressure_recent_paths=()
  local warm_entry=0
  for d in "$target_root"/*; do
    [ -d "$d" ] || continue
    scanned=$((scanned + 1))
    name="$(basename "$d")"
    warm_entry=0
    if entry_requested "$name"; then
      matched_entries+=("$name")
    fi

    # The caller's own entry outranks every other rule, including the process
    # probe: a setup run that has restored a seed but not yet started Cargo owns
    # its tree without holding a lock or naming it on any command line.
    if entry_protected "$name"; then
      echo "keep (caller protected entry): $name"
      report_pending_named_entry "$name"
      kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi

    if grep -qxF "$name" "$keep"; then
      # A live worktree points at this tree, so it is warm rather than an
      # orphan. It is not decided here: the retention cap below ranks every
      # warm entry in this root against the others.
      warm_entry=1
    fi

    # Liveness is decided by process. An age bound alone cannot answer it: a
    # build paused between steps leaves its tree untouched and reads as idle.
    if [ "$probe_mode" != "process" ]; then
      echo "keep (process ownership unmeasured): $name"
      report_pending_named_entry "$name"
      pending_candidates=$((pending_candidates + 1))
      kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi
    if ! pids="$(entry_live_pids "$d" | sort -u | tr '\n' ' ')"; then
      echo "keep (process ownership unmeasured): $name"
      report_pending_named_entry "$name"
      pending_candidates=$((pending_candidates + 1))
      kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi
    pids="${pids% }"
    if [ -n "$pids" ]; then
      echo "keep (live process: $pids): $name"
      report_pending_named_entry "$name"
      kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi

    # Placed after the liveness probe and before the rank and age rules on
    # purpose. A live process still wins, because the caller can be wrong about
    # a lane being finished but cannot be wrong about a compiler holding the
    # tree open. Rank and age are exactly what the caller is overriding.
    if entry_requested "$name"; then
      remove_entry "$target_root" "$d" "named by caller"
      continue
    fi

    if [ "$warm_entry" -eq 1 ]; then
      # An mtime this run could not read is not a rank. Keep it outright
      # rather than letting an unreadable tree sort to the bottom and be
      # retired for a measurement that never happened.
      if ! m="$(target_entry_activity_epoch "$d")"; then
        echo "keep (live worktree, mtime unavailable): $name"
        kept=$((kept + 1)); kept_paths+=("$d"); continue
      fi
      printf '%s %s\n' "$m" "$d" >> "$warm_file"
      continue
    fi

    # The age bound stays, as a staleness rule rather than a liveness one. It
    # is the last line of defence when worktree discovery is incomplete -- a
    # root that is not in HARN_TARGET_GC_ROOTS has no keep-set entry, and a
    # warm target for such a worktree must still survive.
    if ! m="$(target_entry_activity_epoch "$d")"; then
      echo "keep (mtime unavailable): $name"; kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi
    if [ "$m" -ge "$cutoff" ]; then
      echo "keep (recently active): $name"
      kept=$((kept + 1)); kept_paths+=("$d"); continue
    fi

    remove_entry "$target_root" "$d" "orphan"
  done

  # Second pass: the retention cap, over the entries the first pass kept only
  # because a live worktree still points at them. Rank by recency, keep the
  # newest few whatever their age, and retire the rest once they pass the idle
  # bound. A live process already won in the first pass and never reaches
  # here, and an entry whose mtime could not be read was never recorded, so
  # the cap can only choose among trees this run has measured.
  if [ "$keep_recent" -ge 0 ] && [ -s "$warm_file" ]; then
    local rank=0 mtime path base
    while IFS=' ' read -r mtime path; do
      [ -n "$path" ] || continue
      rank=$((rank + 1))
      base="$(basename "$path")"
      if [ "$rank" -le "$keep_recent" ]; then
        pressure_recent_paths+=("$path")
        echo "keep (within the $keep_recent most recent): $base"
        kept=$((kept + 1)); kept_paths+=("$path")
        printf '%s %s\n' "$mtime" "$path" >> "$evictable_file"
        continue
      fi
      if [ "$mtime" -ge "$idle_cutoff" ]; then
        echo "keep (past the $keep_recent most recent but built within ${max_idle}s): $base"
        kept=$((kept + 1)); kept_paths+=("$path")
        printf '%s %s\n' "$mtime" "$path" >> "$evictable_file"
        continue
      fi
      remove_entry "$target_root" "$path" "cold cache"
    done < <(sort -rn "$warm_file")
  fi

  enforce_storage_bounds "$target_root"
}

# Drop one path from the kept list. The size pass runs after entries have
# already been recorded as kept, and the run's final read-back asserts that
# every kept path still exists. Without this the ceiling's own eviction would
# trip that check and report a correct run as an error.
forget_kept_path() {
  local drop="$1" keptp rebuilt=()
  for keptp in ${kept_paths[@]+"${kept_paths[@]}"}; do
    [ "$keptp" = "$drop" ] && continue
    rebuilt+=("$keptp")
  done
  kept_paths=()
  [ "${#rebuilt[@]}" -gt 0 ] && kept_paths=("${rebuilt[@]}")
  return 0
}

remember_kept_path() {
  local path="$1" keptp
  # Bash 3.2 treats an empty "${array[@]}" as unbound under `set -u`, and the
  # first kept path of a run arrives here with the array still empty.
  for keptp in ${kept_paths[@]+"${kept_paths[@]}"}; do
    [ "$keptp" = "$path" ] && return 0
  done
  kept=$((kept + 1))
  kept_paths+=("$path")
}

# Bytes on disk for one entry, or empty when it cannot be measured.
entry_kib() {
  local kib
  kib="$(du -sk "$1" 2>/dev/null | awk 'NR == 1 { print $1 }')"
  case "$kib" in
    ''|*[!0-9]*) return 1 ;;
  esac
  printf '%s\n' "$kib"
}

# Enforce both storage bounds in the existing oldest-first eviction pass.
enforce_storage_bounds() {
  local target_root="$1"
  local min_free_bytes="$min_free_bytes"
  if [ "$max_bytes" -eq 0 ] && [ "$min_free_bytes" -eq 0 ]; then return 0; fi

  # Measure every entry the run kept, protected ones included: the ceiling is a
  # statement about the root's total size, and ignoring the protected entries
  # would let a root sit permanently over it while this pass evicted the rest.
  local total_kib=0 kib path mtime measured=0 unmeasured=0 space available_kib=""
  for path in ${kept_paths[@]+"${kept_paths[@]}"}; do
    case "$path" in "$target_root"/*) ;; *) continue ;; esac
    if kib="$(entry_kib "$path")"; then
      total_kib=$((total_kib + kib)); measured=$((measured + 1))
    else
      unmeasured=$((unmeasured + 1))
    fi
  done
  local ceiling_kib=$((max_bytes / 1024))
  if [ "$min_free_bytes" -gt 0 ]; then
    if space="$(filesystem_space "$target_root")"; then
      min_free_bytes="$(pressure_floor_bytes "${space%% *}")"
      available_kib="${space#* }"
      echo "free space before: root=$target_root available_bytes=$((available_kib * 1024)) floor_bytes=$min_free_bytes"
    else
      echo "free space before: root=$target_root unavailable" >&2
    fi
  fi
  local floor_kib=$(((min_free_bytes + 1023) / 1024))
  if [ "$max_bytes" -gt 0 ]; then
    echo "size ceiling: root holds ${total_kib}KiB in ${measured} measured entr(y|ies), ceiling ${ceiling_kib}KiB"
  fi
  # An entry this run could not measure is not a zero. Say so rather than
  # letting a failed `du` read as headroom.
  if [ "$unmeasured" -gt 0 ]; then
    echo "size ceiling: ${unmeasured} kept entr(y|ies) could not be measured; the total above is a lower bound" >&2
  fi
  # Coldest first. `sort -n` puts the oldest activity stamp at the top, which
  # is the opposite order from the retention cap above and is deliberate.
  local over_size under_pressure reason removed_before
  while IFS=' ' read -r mtime path; do
    [ -n "$path" ] || continue
    over_size=0; under_pressure=0
    if [ "$max_bytes" -gt 0 ] && [ "$total_kib" -gt "$ceiling_kib" ]; then over_size=1; fi
    if [ -n "$available_kib" ] && [ "$available_kib" -lt "$floor_kib" ]; then under_pressure=1; fi
    if [ "$over_size" -eq 0 ] && [ "$under_pressure" -eq 0 ]; then break; fi
    case "$path" in "$target_root"/*) ;; *) continue ;; esac
    [ -d "$path" ] || continue
    if [ "$min_free_bytes" -gt 0 ] && pressure_protected "$path"; then
      echo "keep (recent working set; storage grace): $(basename "$path")"
      continue
    fi
    if [ "$under_pressure" -eq 1 ] && [ "$probe_mode" != process ]; then
      echo "keep (process ownership unmeasured; pressure grace): $(basename "$path")"
      continue
    fi
    if [ "$mtime" -ge "$cutoff" ]; then
      echo "keep (recently active within ${min_age}s; size grace): $(basename "$path")"
      continue
    fi
    kib="$(entry_kib "$path")" || continue
    reason="over the size ceiling"
    [ "$under_pressure" -eq 0 ] || reason="below the free-space floor"
    removed_before="$removed"
    remove_entry "$target_root" "$path" "$reason"
    # Only count the space back when the entry is really gone. Under --dry-run
    # nothing is removed, but the projection must still advance or the loop
    # would report every entry as needing removal.
    if [ "$removed" -gt "$removed_before" ]; then
      total_kib=$((total_kib - kib))
      kept=$((kept - 1))
      forget_kept_path "$path"
      if [ "$dry_run" -eq 1 ]; then
        if [ -n "$available_kib" ]; then available_kib=$((available_kib + kib)); fi
      elif [ "$min_free_bytes" -gt 0 ]; then
        if space="$(filesystem_space "$target_root")"; then
          available_kib="${space#* }"
        else
          available_kib=""
        fi
      fi
    fi
  done < <(sort -n "$evictable_file")
  if [ "$max_bytes" -gt 0 ]; then
    echo "size ceiling: root now holds ${total_kib}KiB against ceiling ${ceiling_kib}KiB"
    if [ "$total_kib" -gt "$ceiling_kib" ] || [ "$unmeasured" -gt 0 ]; then
      ceiling_unproven_roots=$((ceiling_unproven_roots + 1))
      echo "size ceiling: cannot prove this root fits while protected or unmeasured entries remain" >&2
    fi
  fi
}

entry_holds_git_repository() {
  local hit status
  hit="$(find "$1" \( -name .git -o \( -type d -path '*/objects/pack' \) \) -print -quit 2>/dev/null)"
  status=$?
  [ -n "$hit" ] && return 0
  [ "$status" -eq 0 ] || return 0
  return 1
}

# Remove one entry, or report what would happen under --dry-run. Both passes
# route through here so the path validation and the read-back are written once
# and cannot drift apart.
remove_entry() {
  local target_root="$1" d="$2" reason="$3"
  local name sz kib=""
  name="$(basename "$d")"
  sz=$(du -sh "$d" 2>/dev/null | cut -f1 || true)
  if [ "$measure_bytes" -eq 1 ]; then
    if ! kib="$(entry_kib "$d")"; then
      unmeasured_entries=$((unmeasured_entries + 1))
      kib=""
    fi
  fi
  # Entries are Cargo build output, which never holds a git repository. A clone
  # other clones borrow objects from (`git clone --reference`) breaks all of
  # them when it is deleted, which happened on a fleet host on 2026-09-27, so
  # an entry holding a `.git` or a bare object store is refused whatever rule
  # selected it. An entry the census cannot fully read is refused too: the
  # absence of a repository is then unproven.
  if entry_holds_git_repository "$d"; then
    echo "refusing to remove $reason: $name holds a git repository or could not be fully read" >&2
    remember_kept_path "$d"
    pending_candidates=$((pending_candidates + 1))
    return 0
  fi
  # Discovery and size measurement can outlast a build's startup.
  if [ "$probe_mode" != process ]; then
    echo "keep (process ownership unmeasured before removal): $name" >&2
    remember_kept_path "$d"
    report_pending_named_entry "$name"
    pending_candidates=$((pending_candidates + 1))
    return 0
  fi
  local pids
  if ! pids="$(entry_live_pids "$d" | sort -u | tr '\n' ' ')"; then
    echo "keep (process ownership unmeasured before removal): $name" >&2
    remember_kept_path "$d"
    report_pending_named_entry "$name"
    pending_candidates=$((pending_candidates + 1))
    return 0
  fi
  if [ -n "$pids" ]; then
    echo "keep (live process before removal: $pids): $name"
    remember_kept_path "$d"
    report_pending_named_entry "$name"
    return 0
  fi
  if [ "$dry_run" -eq 1 ]; then
    echo "would remove $reason: $name (${sz:-?})"
    removed=$((removed + 1))
    return 0
  fi

  # Remove one exact path built from the validated root. Nothing here expands
  # a pattern, and nothing outside the root is reachable.
  if [ "$(dirname "$d")" != "$target_root" ] || [ "$name" = "." ] || [ "$name" = ".." ]; then
    echo "refusing to remove a path outside the managed root: $d" >&2
    remember_kept_path "$d"
    pending_candidates=$((pending_candidates + 1))
    return 0
  fi
  echo "removing $reason: $name (${sz:-?})"
  rm -rf "$d" || true
  # Removal is destructive, so read the decision back instead of trusting the
  # exit status of an `rm` that is deliberately failure-tolerant.
  if [ -e "$d" ]; then
    echo "warning: $reason survived removal, still present: $d" >&2
    remember_kept_path "$d"
    pending_candidates=$((pending_candidates + 1))
    return 0
  fi
  removed=$((removed + 1))
  if [ -n "$kib" ]; then
    reclaimed_bytes=$((reclaimed_bytes + kib * 1024))
  fi
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

if [ "$measure_bytes" -eq 1 ] && [ "${#kept_paths[@]}" -gt 0 ]; then
  for kept_path in "${kept_paths[@]}"; do
    if kib="$(entry_kib "$kept_path")"; then
      retained_bytes=$((retained_bytes + kib * 1024))
    else
      unmeasured_entries=$((unmeasured_entries + 1))
    fi
  done
fi

if [ "$min_free_bytes" -gt 0 ]; then
  for target_root in ${walked_roots[@]+"${walked_roots[@]}"}; do
    if space="$(filesystem_space "$target_root")"; then
      available_kib="${space#* }"
      root_floor_bytes="$(pressure_floor_bytes "${space%% *}")"
      echo "free space after: root=$target_root available_bytes=$((available_kib * 1024)) floor_bytes=$root_floor_bytes"
      if [ $((available_kib * 1024)) -ge "$root_floor_bytes" ]; then continue; fi
    else
      echo "free space after: root=$target_root unavailable" >&2
    fi
    pressure_unproven_roots=$((pressure_unproven_roots + 1))
  done
fi

record_success
completed=1
print_summary
