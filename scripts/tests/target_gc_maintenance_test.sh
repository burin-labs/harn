#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
maintenance="$repo_root/scripts/target_gc_maintenance.sh"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-target-gc-maintenance-test.XXXXXX")"
trap 'rm -rf -- "$test_root"' EXIT

bare="$test_root/current.git"
source_checkout="$test_root/stale-checkout"
mkdir -p "$test_root/no-repos"
git clone --shared --bare -q "$repo_root" "$bare"
git --git-dir "$bare" update-ref refs/heads/main "$(git -C "$repo_root" rev-parse HEAD)"
git clone -q "$bare" "$source_checkout"

# A checkout-local collector would print this stale marker. The wrapper must
# fetch and execute the current main collector even though this file is old.
printf '#!/usr/bin/env bash\necho stale-checkout-collector\n' \
  > "$source_checkout/scripts/prune_stale_targets.sh"
HARN_TARGET_GC_SOURCE_REPO="$source_checkout" \
HARN_DEV_SETUP_STORAGE_ROOT="$test_root/storage" \
HARN_TARGET_GC_ROOTS="$test_root/no-repos" \
HARN_TARGET_GC_MAX_BYTES=1048576 \
TMPDIR="$test_root" \
PATH=/usr/bin:/bin \
  "$maintenance" --dry-run > "$test_root/current-output.txt" 2>&1
if grep -Fq stale-checkout-collector "$test_root/current-output.txt" \
  || ! grep -Eq 'policy=harn-target-gc/v2-[^ ]+ status=dry-run .*scanned=0' \
    "$test_root/current-output.txt"; then
  echo "maintenance executed a stale policy or hid its empty scan" >&2
  cat "$test_root/current-output.txt" >&2
  exit 1
fi

# If fresh policy cannot be fetched, a stale local file must never be used as
# a fallback. That would turn a network outage into silent policy regression.
git -C "$source_checkout" remote set-url origin "$test_root/missing.git"
if HARN_TARGET_GC_SOURCE_REPO="$source_checkout" \
    HARN_DEV_SETUP_STORAGE_ROOT="$test_root/storage" \
    HARN_TARGET_GC_ROOTS="$test_root/no-repos" \
    TMPDIR="$test_root" \
    "$maintenance" --dry-run > "$test_root/fetch-failure.txt" 2>&1; then
  echo "maintenance treated a failed fetch as a successful sweep" >&2
  exit 1
fi
if grep -Fq 'harn-target GC:' "$test_root/fetch-failure.txt"; then
  echo "maintenance ran a collector after its fetch failed" >&2
  cat "$test_root/fetch-failure.txt" >&2
  exit 1
fi

# Installation preserves unrelated jobs and does not duplicate its own entry
# when setup is repeated. The fake crontab keeps this test off the host timer.
mkdir -p "$test_root/fake-bin"
cat > "$test_root/fake-bin/crontab" <<'CRONTAB'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  -l) cat "$HARN_TARGET_GC_TEST_CRONTAB" ;;
  -)
    if [ "${HARN_TARGET_GC_TEST_CRON_WRITE_DENY:-0}" = 1 ]; then
      echo 'crontab: Operation not permitted' >&2
      exit 1
    fi
    if [ "${HARN_TARGET_GC_TEST_CRON_WRITE_SILENT:-0}" = 1 ]; then
      cat > /dev/null
      exit 0
    fi
    cat > "$HARN_TARGET_GC_TEST_CRONTAB" ;;
  *) exit 2 ;;
esac
CRONTAB
chmod +x "$test_root/fake-bin/crontab"
printf '%s\n' '0 1 * * * existing-job' > "$test_root/crontab"
for attempt in 1 2; do
  PATH="$test_root/fake-bin:$PATH" \
  HARN_TARGET_GC_TEST_CRONTAB="$test_root/crontab" \
  HARN_TARGET_GC_INSTALL_DIR="$test_root/installed" \
  HARN_TARGET_GC_LOG_DIR="$test_root/logs" \
    "$repo_root/scripts/install_target_gc_maintenance.sh" \
      > "$test_root/install-$attempt.txt"
done
if [ "$(grep -Fc '# harn-target-gc-maintenance' "$test_root/crontab")" -ne 1 ] \
  || ! grep -Fq '0 1 * * * existing-job' "$test_root/crontab" \
  || [ ! -x "$test_root/installed/harn-target-gc-maintenance" ]; then
  echo "maintenance installation lost an existing job or duplicated itself" >&2
  cat "$test_root/crontab" >&2
  exit 1
fi

# A write may fail explicitly or appear to succeed without installing a job.
# Both must fail instead of reporting a scheduled collector.
for failure in DENY SILENT; do
  printf '%s\n' '0 1 * * * existing-job' > "$test_root/crontab"
  flag="HARN_TARGET_GC_TEST_CRON_WRITE_$failure"
  if env PATH="$test_root/fake-bin:$PATH" \
      HARN_TARGET_GC_TEST_CRONTAB="$test_root/crontab" \
      HARN_TARGET_GC_INSTALL_DIR="$test_root/installed" \
      HARN_TARGET_GC_LOG_DIR="$test_root/logs" \
      "$flag=1" "$repo_root/scripts/install_target_gc_maintenance.sh" \
      > "$test_root/cron-$failure.txt" 2>&1; then
    echo "maintenance accepted a $failure cron write" >&2
    exit 1
  fi
done

# Fake only the scheduler boundary. A macOS user session can read cron yet be
# forbidden to write it, while its launchd user domain accepts a LaunchAgent.
cat > "$test_root/fake-bin/uname" <<'UNAME'
#!/usr/bin/env bash
printf '%s\n' "${HARN_TARGET_GC_TEST_PLATFORM:-Linux}"
UNAME
cat > "$test_root/fake-bin/plutil" <<'PLUTIL'
#!/usr/bin/env bash
test "$1" = -lint
grep -Fq '<plist version="1.0">' "$2"
PLUTIL
cat > "$test_root/fake-bin/launchctl" <<'LAUNCHCTL'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  print)
    [ "${HARN_TARGET_GC_TEST_LAUNCHCTL_HIDE:-0}" != 1 ]
    [ -f "$HARN_TARGET_GC_TEST_LAUNCHCTL_STATE" ]
    printf 'path = %s\nstate = not running\n' "$(cat "$HARN_TARGET_GC_TEST_LAUNCHCTL_STATE")" ;;
  bootstrap)
    printf '%s' "$3" > "$HARN_TARGET_GC_TEST_LAUNCHCTL_STATE"
    printf 'bootstrap\n' >> "$HARN_TARGET_GC_TEST_LAUNCHCTL_CALLS" ;;
  bootout)
    rm -f -- "$HARN_TARGET_GC_TEST_LAUNCHCTL_STATE"
    printf 'bootout\n' >> "$HARN_TARGET_GC_TEST_LAUNCHCTL_CALLS" ;;
  *) exit 2 ;;
esac
LAUNCHCTL
chmod +x "$test_root/fake-bin/uname" "$test_root/fake-bin/plutil" \
  "$test_root/fake-bin/launchctl"

mac_home="$test_root/macos home&"
mkdir -p "$mac_home"
mac_plist="$mac_home/Library/LaunchAgents/com.harn.target-gc-maintenance.plist"
mac_crontab="$test_root/macos-crontab"
printf '%s\n' '0 1 * * * existing-job' \
  '17 3 * * * old-maintenance # harn-target-gc-maintenance' > "$mac_crontab"
for attempt in 1 2; do
  HOME="$mac_home" \
  PATH="$test_root/fake-bin:$PATH" \
  HARN_TARGET_GC_TEST_PLATFORM=Darwin \
  HARN_TARGET_GC_TEST_CRONTAB="$mac_crontab" \
  HARN_TARGET_GC_TEST_LAUNCHCTL_STATE="$test_root/launchctl-state" \
  HARN_TARGET_GC_TEST_LAUNCHCTL_CALLS="$test_root/launchctl-calls" \
    "$repo_root/scripts/install_target_gc_maintenance.sh" \
      > "$test_root/mac-install-$attempt.txt"
done
if [ "$(grep -Fc bootstrap "$test_root/launchctl-calls")" -ne 1 ] \
  || grep -Fq '# harn-target-gc-maintenance' "$mac_crontab" \
  || ! grep -Fq '0 1 * * * existing-job' "$mac_crontab" \
  || ! grep -Fq 'macos home&amp;' "$mac_plist" \
  || [ ! -x "$mac_home/.local/bin/harn-target-gc-maintenance" ]; then
  echo "macOS install duplicated its service, kept legacy cron, or lost another job" >&2
  exit 1
fi

# Once migrated, a denied cron write is irrelevant: the installer must not
# touch cron when its marker is absent. A missing launchd readback still fails.
HOME="$mac_home" \
PATH="$test_root/fake-bin:$PATH" \
HARN_TARGET_GC_TEST_PLATFORM=Darwin \
HARN_TARGET_GC_TEST_CRONTAB="$mac_crontab" \
HARN_TARGET_GC_TEST_CRON_WRITE_DENY=1 \
HARN_TARGET_GC_TEST_LAUNCHCTL_STATE="$test_root/launchctl-state" \
HARN_TARGET_GC_TEST_LAUNCHCTL_CALLS="$test_root/launchctl-calls" \
  "$repo_root/scripts/install_target_gc_maintenance.sh" \
    > "$test_root/mac-denied-cron.txt"
if HOME="$mac_home" \
    PATH="$test_root/fake-bin:$PATH" \
    HARN_TARGET_GC_TEST_PLATFORM=Darwin \
    HARN_TARGET_GC_TEST_CRONTAB="$mac_crontab" \
    HARN_TARGET_GC_TEST_LAUNCHCTL_HIDE=1 \
    HARN_TARGET_GC_TEST_LAUNCHCTL_STATE="$test_root/launchctl-state" \
    HARN_TARGET_GC_TEST_LAUNCHCTL_CALLS="$test_root/launchctl-calls" \
      "$repo_root/scripts/install_target_gc_maintenance.sh" \
        > "$test_root/mac-hidden-readback.txt" 2>&1; then
  echo "macOS install accepted a missing LaunchAgent readback" >&2
  exit 1
fi

# If a legacy cron marker cannot be removed, roll back the LaunchAgent so the
# host cannot run two daily collectors. The old cron entry stays in place.
printf '%s\n' '0 1 * * * existing-job' \
  '17 3 * * * old-maintenance # harn-target-gc-maintenance' > "$mac_crontab"
if HOME="$mac_home" \
    PATH="$test_root/fake-bin:$PATH" \
    HARN_TARGET_GC_TEST_PLATFORM=Darwin \
    HARN_TARGET_GC_TEST_CRONTAB="$mac_crontab" \
    HARN_TARGET_GC_TEST_CRON_WRITE_DENY=1 \
    HARN_TARGET_GC_TEST_LAUNCHCTL_STATE="$test_root/launchctl-state" \
    HARN_TARGET_GC_TEST_LAUNCHCTL_CALLS="$test_root/launchctl-calls" \
      "$repo_root/scripts/install_target_gc_maintenance.sh" \
        > "$test_root/mac-legacy-denied.txt" 2>&1; then
  echo "macOS install accepted a duplicate schedule after cron refusal" >&2
  exit 1
fi
if [ -f "$test_root/launchctl-state" ] || [ -e "$mac_plist" ] \
  || ! grep -Fq '# harn-target-gc-maintenance' "$mac_crontab"; then
  echo "macOS install kept a second schedule after cron refusal" >&2
  exit 1
fi

echo "target_gc_maintenance_test: ok"
