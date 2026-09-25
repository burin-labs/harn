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
  -) cat > "$HARN_TARGET_GC_TEST_CRONTAB" ;;
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

echo "target_gc_maintenance_test: ok"
