#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export HARN_TARGET_GC_HANDLE_PROBE="${HARN_TARGET_GC_HANDLE_PROBE:-$repo_root/scripts/tests/fixtures/target-gc-handle-probe.sh}"
gc="$repo_root/scripts/prune_stale_targets.sh"
tmp_root="$(mktemp -d)"
holder=""
late_holder=""
cleanup() {
  local pid
  for pid in "$holder" "$late_holder"; do
    [ -n "$pid" ] || continue
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  rm -rf "$tmp_root"
}
trap cleanup EXIT
fail() { echo "prune_stale_targets_pressure_test: $1" >&2; cat "$tmp_root/run.log" >&2; exit 1; }

storage="$tmp_root/storage"
repos="$tmp_root/repos"
targets="$storage/harn-target"
mkdir -p "$targets" "$repos" "$tmp_root/bin"
git -C "$repos" init -b main -q main
git -C "$repos/main" config user.email gc@example.invalid
git -C "$repos/main" config user.name gc
git -C "$repos/main" commit -q --allow-empty -m seed
export TZ=UTC
minute=50
for lane in recent-a recent-b middle cold-b cold-a held; do
  git -C "$repos/main" worktree add -q "$repos/$lane" -b "$lane"
  mkdir -p "$targets/repos-$lane/debug"
  dd if=/dev/urandom of="$targets/repos-$lane/debug/probe.o" bs=4096 count=1 2>/dev/null
  stamp="$(printf '2020010120%02d' "$minute")"
  touch -t "$stamp" "$targets/repos-$lane/debug" "$targets/repos-$lane"
  minute=$((minute - 10))
done

held="$targets/repos-held"
: > "$held/debug/.cargo-lock"
mkfifo "$tmp_root/ready" "$tmp_root/hold"
(
  exec 9<"$held/debug/.cargo-lock"
  printf 'ready\n' > "$tmp_root/ready"
  IFS= read -r ignored < "$tmp_root/hold"
) &
holder="$!"
IFS= read -r ready < "$tmp_root/ready"
[ "$ready" = ready ] || fail "the live-owner control did not start"

real_date="$(command -v date)"
real_du="$(command -v du)"
cat > "$tmp_root/bin/date" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = +%s ]; then printf '1577923200\n'; else exec "$TEST_REAL_DATE" "$@"; fi
SH
cat > "$tmp_root/bin/df" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$TEST_FREE_MODE" in
  ample) available=500000000 ;;
  low|late)
    remaining="$(find "$TEST_TARGET_ROOT" -mindepth 1 -maxdepth 1 -type d | wc -l)"
    available=$((90000000 + (6 - remaining) * 6000000)) ;;
  blocked) available=90000000 ;;
  unavailable) exit 1 ;;
  *) exit 2 ;;
esac
printf '%s\n' "$available" >> "$TEST_DF_READS"
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'fixture 2000000000 1000000000 %s 50%% /fixture\n' "$available"
SH
cat > "$tmp_root/bin/du" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$TEST_FREE_MODE" = late ] && [ "${1:-}" = -sk ] && [ "${2:-}" = "$TEST_LATE_TARGET" ] \
  && [ ! -e "$TEST_LATE_SENT" ]; then
  : > "$TEST_LATE_SENT"
  printf 'start\n' > "$TEST_LATE_GATE"
  IFS= read -r ready < "$TEST_LATE_READY"
  [ "$ready" = ready ] || exit 1
fi
exec "$TEST_REAL_DU" "$@"
SH
chmod +x "$tmp_root/bin/date" "$tmp_root/bin/df" "$tmp_root/bin/du"

run_gc() {
  PATH="$tmp_root/bin:$PATH" TEST_REAL_DATE="$real_date" TEST_REAL_DU="$real_du" \
    TEST_LATE_TARGET="$targets/repos-late" TEST_LATE_SENT="$tmp_root/late-started" \
    TEST_LATE_GATE="$tmp_root/late-gate" TEST_LATE_READY="$tmp_root/late-ready" \
    TEST_TARGET_ROOT="$targets" TEST_DF_READS="$tmp_root/df-reads" \
    HARN_DEV_SETUP_STORAGE_ROOT="$storage" HARN_TARGET_GC_ROOTS="$repos" \
    HARN_TARGET_GC_KEEP_RECENT=2 HARN_TARGET_GC_MAX_BYTES="${TEST_MAX_BYTES:-0}" \
    HARN_TARGET_GC_MIN_FREE_BYTES=102400000000 \
    bash "$gc" --host-maintenance "$@" > "$tmp_root/run.log" 2>&1
}

# The same warm working set must survive when measured headroom is plentiful.
TEST_FREE_MODE=ample run_gc || fail "the ample-space control failed"
grep -q 'removed=0 ' "$tmp_root/run.log" || fail "ample space evicted a warm entry"
grep -E '^(free space (before|after):|harn-target GC:)' "$tmp_root/run.log"
for lane in recent-a recent-b middle cold-b cold-a held; do
  [ -d "$targets/repos-$lane" ] || fail "ample space removed $lane"
done

# All entries are younger than the 72-hour idle limit; only pressure can act.
TEST_FREE_MODE=low run_gc || fail "the pressure run failed"
for lane in cold-a cold-b; do
  [ ! -e "$targets/repos-$lane" ] || fail "pressure did not evict the oldest eligible key: $lane"
done
for lane in recent-a recent-b middle held; do
  [ -d "$targets/repos-$lane" ] || fail "pressure removed a protected or no-longer-needed candidate: $lane"
done
grep -q 'removed=2 ' "$tmp_root/run.log" || fail "pressure did not stop after reaching the floor"
grep -q '^102000000$' "$tmp_root/df-reads" || fail "free space was not read back after removal"
grep -q 'keep (live process' "$tmp_root/run.log" || fail "the live-owner control was not reached"
grep -E '^(free space (before|after):|harn-target GC:)' "$tmp_root/run.log"

# A build acquires its lock during size measurement, after the initial census.
git -C "$repos/main" worktree add -q "$repos/late" -b late
mkdir -p "$targets/repos-late/debug"
: > "$targets/repos-late/debug/.cargo-lock"
dd if=/dev/urandom of="$targets/repos-late/debug/probe.o" bs=4096 count=1 2>/dev/null
touch -t 202001011900 "$targets/repos-late/debug" "$targets/repos-late"
mkfifo "$tmp_root/late-gate" "$tmp_root/late-ready"
(
  IFS= read -r start < "$tmp_root/late-gate"
  exec 9<"$targets/repos-late/debug/.cargo-lock"
  printf 'ready\n' > "$tmp_root/late-ready"
  IFS= read -r ignored < "$tmp_root/hold"
) &
late_holder="$!"
TEST_FREE_MODE=late run_gc || fail "the late-owner pressure run failed"
[ -d "$targets/repos-late" ] || fail "pressure removed a build that started after discovery"
[ ! -e "$targets/repos-middle" ] || fail "pressure did not continue to the next idle candidate"
grep -q 'keep (live process before removal:' "$tmp_root/run.log" || fail "deletion-time ownership was not reached"
grep -q 'removed=1 ' "$tmp_root/run.log" || fail "the late-owner run did not stop at the floor"
grep -E '^(free space (before|after):|harn-target GC:)' "$tmp_root/run.log"

# Protected keys cannot make an unmet pressure bound look like success.
printf '2000-01-01T00:00:00Z\nfixture-old-policy\n' > "$storage/prune-stale-targets.last-success"
receipt="$(cat "$storage/prune-stale-targets.last-success")"
TEST_FREE_MODE=blocked run_gc --protect-entry repos-late || fail "protected pressure must report a partial pass"
grep -q 'status=partial ' "$tmp_root/run.log" || fail "an unmet free-space floor reported complete"
[ "$(cat "$storage/prune-stale-targets.last-success")" = "$receipt" ] || fail "an unmet floor advanced last success"
grep -E '^(free space (before|after):|harn-target GC:)' "$tmp_root/run.log"
TEST_FREE_MODE=unavailable run_gc --dry-run || fail "unavailable headroom must remain inspectable"
grep -q 'pressure_unproven_roots=1' "$tmp_root/run.log" || fail "a failed free-space read became measured headroom"
grep -E '^(free space (before|after):|harn-target GC:)' "$tmp_root/run.log"

# A size ceiling cannot retire the protected working set after headroom recovers.
TEST_FREE_MODE=ample TEST_MAX_BYTES=1 run_gc --protect-entry repos-late || fail "a protected size bound must remain partial"
for lane in recent-a recent-b held late; do
  [ -d "$targets/repos-$lane" ] || fail "the size bound removed protected working set $lane"
done
grep -q 'status=partial ' "$tmp_root/run.log" || fail "an unmet size ceiling reported complete"
grep -q 'ceiling_unproven_roots=1' "$tmp_root/run.log" || fail "the size ceiling control was not reached"
[ "$(cat "$storage/prune-stale-targets.last-success")" = "$receipt" ] || fail "an unmet size ceiling advanced last success"
grep -E '^(free space (before|after):|harn-target GC:)' "$tmp_root/run.log"

echo "prune_stale_targets_pressure_test: ok"
