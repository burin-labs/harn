#!/bin/sh
set -eu

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/harn-bootstrap-seed-test.XXXXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
fixture=$scratch/fixture
tools=$scratch/tools
mkdir -p "$fixture/archive" "$tools"

cat > "$fixture/archive/harn" <<'SEED'
#!/bin/sh
exit "${HARN_SEED_TEST_EXIT_CODE:-0}"
SEED
chmod +x "$fixture/archive/harn"
tar -cJf "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz" -C "$fixture/archive" harn
if command -v sha256sum >/dev/null 2>&1; then
  checksum=$(sha256sum "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz")
else
  checksum=$(shasum -a 256 "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz")
fi
checksum=${checksum%% *}
printf '%s  %s\n' "$checksum" harn-x86_64-unknown-linux-gnu.tar.xz > "$fixture/SHA256SUMS"
printf '%s\n' '{"assets":[{"name":"SHA256SUMS","id":101},{"name":"harn-x86_64-unknown-linux-gnu.tar.xz","id":102}]}' > "$fixture/release.json"

cat > "$tools/uname" <<'EOF'
#!/bin/sh
case "$1" in
  -m) printf '%s\n' x86_64 ;;
  -s) printf '%s\n' Linux ;;
  *) exit 2 ;;
esac
EOF
cat > "$tools/curl" <<'EOF'
#!/bin/sh
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) output=$2; shift 2 ;;
    http*) url=$1; shift ;;
    *) shift ;;
  esac
done
if [ "${HARN_SEED_TEST_API_FALLBACK:-0}" = 1 ]; then
  printf '%s\n' "$url" >> "$HARN_SEED_TEST_API_LOG"
  case "$url" in
    https://github.com/*) exit 22 ;;
    */releases/tags/*) cp "$HARN_SEED_TEST_FIXTURE/release.json" "$output"; exit ;;
    */releases/assets/101) cp "$HARN_SEED_TEST_FIXTURE/SHA256SUMS" "$output"; exit ;;
    */releases/assets/102) cp "$HARN_SEED_TEST_FIXTURE/harn-x86_64-unknown-linux-gnu.tar.xz" "$output"; exit ;;
    *) exit 1 ;;
  esac
fi
if [ -n "${HARN_SEED_TEST_OUTAGES:-}" ]; then
  # Simulate a release host that answers 5xx for its first N downloads, with
  # the release API down for the whole outage.
  case "$url" in https://api.github.com/*) exit 22 ;; esac
  served=$(cat "$HARN_SEED_TEST_OUTAGES.count" 2>/dev/null || echo 0)
  printf '%s\n' "$((served + 1))" > "$HARN_SEED_TEST_OUTAGES.count"
  [ "$served" -ge "$(cat "$HARN_SEED_TEST_OUTAGES")" ] || exit 22
fi
[ "${HARN_SEED_TEST_INTERRUPT:-0}" != 1 ] || [ "${url##*/}" = SHA256SUMS ] || {
  printf '%s' partial > "$output"
  exit 1
}
cp "$HARN_SEED_TEST_FIXTURE/${url##*/}" "$output"
EOF
chmod +x "$tools/uname" "$tools/curl"

run_seed() {
  PATH="$tools:$PATH" \
  TMPDIR="$scratch" \
  XDG_CACHE_HOME="$scratch/cache-home" \
  HARN_EXT_BOOTSTRAP_SEED_VERSION=9.8.7 \
  HARN_SEED_TEST_FIXTURE="$fixture" \
    sh "$repo/scripts/bootstrap-harn.sh" --help
}

cp "$fixture/SHA256SUMS" "$fixture/SHA256SUMS.valid"
printf '%s\n' malformed >> "$fixture/SHA256SUMS"
if run_seed >/dev/null 2>&1; then
  echo 'malformed seed metadata unexpectedly succeeded' >&2
  exit 1
fi
test ! -e "$scratch/cache-home/harn/bootstrap-seed/9.8.7/x86_64-unknown-linux-gnu/SHA256SUMS"

cp "$fixture/SHA256SUMS.valid" "$fixture/SHA256SUMS"
run_seed >/dev/null
cache=$scratch/cache-home/harn/bootstrap-seed/9.8.7/x86_64-unknown-linux-gnu
cmp "$fixture/SHA256SUMS" "$cache/SHA256SUMS"
cmp "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz" "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"

# Drive the adapter's real EXIT trap with a simulated executable lock. Keep
# the scratch root outside the tool shim so the test can remove leftovers.
real_rm=$(command -v rm)
export HARN_SEED_TEST_REAL_RM="$real_rm"
cat > "$tools/rm" <<'EOF'
#!/bin/sh
for argument do
  case "$argument" in
    */harn-seed.*)
      printf '%s\n' "$argument" > "$HARN_SEED_TEST_LEFTOVER"
      echo 'simulated Windows executable lock' >&2
      exit 1
      ;;
  esac
done
exec "$HARN_SEED_TEST_REAL_RM" "$@"
EOF
chmod +x "$tools/rm"
export HARN_SEED_TEST_LEFTOVER="$scratch/leftover"
if run_seed > "$scratch/locked.stdout" 2> "$scratch/locked.stderr"; then
  status=0
else
  status=$?
fi
[ "$status" = 0 ] || { echo "verified seed failed on cleanup: status=$status" >&2; exit 1; }
leftover=$(cat "$HARN_SEED_TEST_LEFTOVER")
test -f "$leftover/harn"
grep -F "warning: bootstrap seed cleanup left $leftover" "$scratch/locked.stderr" >/dev/null
"$real_rm" -rf "$leftover"

# A seed/verification failure must survive a simultaneous cleanup failure.
export HARN_SEED_TEST_EXIT_CODE=7
if run_seed > "$scratch/failed.stdout" 2> "$scratch/failed.stderr"; then
  status=0
else
  status=$?
fi
[ "$status" = 7 ] || { echo "seed failure was masked by cleanup: status=$status" >&2; exit 1; }
leftover=$(cat "$HARN_SEED_TEST_LEFTOVER")
test -f "$leftover/harn"
grep -F "warning: bootstrap seed cleanup left $leftover" "$scratch/failed.stderr" >/dev/null
"$real_rm" -rf "$leftover"
unset HARN_SEED_TEST_EXIT_CODE

# Corrupt bytes must still fail verification even when cleanup also fails.
cp "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz" "$fixture/archive.valid"
printf '%s\n' corrupt > "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz"
"$real_rm" -f "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"
if run_seed > "$scratch/corrupt.stdout" 2> "$scratch/corrupt.stderr"; then
  echo 'corrupt seed unexpectedly passed verification' >&2
  exit 1
fi
grep -F 'seed checksum mismatch' "$scratch/corrupt.stderr" >/dev/null
leftover=$(cat "$HARN_SEED_TEST_LEFTOVER")
grep -F "warning: bootstrap seed cleanup left $leftover" "$scratch/corrupt.stderr" >/dev/null
test ! -e "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"
"$real_rm" -rf "$leftover"
cp "$fixture/archive.valid" "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz"
"$real_rm" -f "$tools/rm"
run_seed >/dev/null

PATH="$tools:$PATH" \
XDG_CACHE_HOME="$scratch/cache-home" \
HARN_EXT_BOOTSTRAP_SEED_VERSION=9.8.7 \
HARN_BOOTSTRAP_OFFLINE=1 \
HARN_SEED_TEST_FIXTURE="$scratch/absent" \
  sh "$repo/scripts/bootstrap-harn.sh" --help >/dev/null

rm -f "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"
if PATH="$tools:$PATH" XDG_CACHE_HOME="$scratch/cache-home" \
  HARN_EXT_BOOTSTRAP_SEED_VERSION=9.8.7 HARN_SEED_TEST_INTERRUPT=1 \
  HARN_SEED_TEST_FIXTURE="$fixture" sh "$repo/scripts/bootstrap-harn.sh" --help >/dev/null 2>&1; then
  echo 'interrupted seed download unexpectedly succeeded' >&2
  exit 1
fi
test ! -e "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"

# A transient release-host outage is retried within the configured budget and
# fails once that budget is spent.
export HARN_SEED_TEST_OUTAGES="$scratch/outages"
printf '%s\n' 1 > "$HARN_SEED_TEST_OUTAGES"
rm -rf "$scratch/cache-home" "$HARN_SEED_TEST_OUTAGES.count"
if HARN_BOOTSTRAP_MAX_ATTEMPTS=1 run_seed > "$scratch/outage.stdout" 2> "$scratch/outage.stderr"; then
  echo 'seed download unexpectedly survived an outage with one attempt' >&2
  exit 1
fi
grep -F 'seed download failed after 1 attempt(s): SHA256SUMS' "$scratch/outage.stderr" >/dev/null
rm -rf "$scratch/cache-home" "$HARN_SEED_TEST_OUTAGES.count"
HARN_BOOTSTRAP_MAX_ATTEMPTS=2 HARN_BOOTSTRAP_RETRY_DELAY_SECONDS=0 run_seed > /dev/null 2> "$scratch/outage.stderr"
grep -F 'seed download failed (attempt 1/2); retrying in 0s' "$scratch/outage.stderr" >/dev/null
cmp "$fixture/SHA256SUMS" "$cache/SHA256SUMS"
[ "$(cat "$HARN_SEED_TEST_OUTAGES.count")" = 3 ]
unset HARN_SEED_TEST_OUTAGES
echo 'Seed outage retry: one-attempt failure and two-attempt recovery passed.'

if command -v jq >/dev/null 2>&1; then
  rm -rf "$scratch/cache-home"
  export HARN_SEED_TEST_API_FALLBACK=1 HARN_SEED_TEST_API_LOG="$scratch/api.log"
  run_seed >/dev/null
  cmp "$fixture/SHA256SUMS" "$cache/SHA256SUMS"
  cmp "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz" "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"
  [ "$(grep -c '/releases/download/' "$HARN_SEED_TEST_API_LOG")" = 1 ]
  [ "$(grep -c '/releases/tags/' "$HARN_SEED_TEST_API_LOG")" = 1 ]
  [ "$(grep -c '/releases/assets/' "$HARN_SEED_TEST_API_LOG")" = 2 ]
  rm -f "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"
  cp "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz" "$fixture/archive.valid"
  printf '%s\n' corrupt > "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz"
  if run_seed > "$scratch/api-corrupt.stdout" 2> "$scratch/api-corrupt.stderr"; then
    echo 'corrupt API fallback seed unexpectedly passed verification' >&2
    exit 1
  fi
  grep -F 'seed checksum mismatch' "$scratch/api-corrupt.stderr" >/dev/null
  test ! -e "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"
  cp "$fixture/archive.valid" "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz"
  cp "$fixture/release.json" "$fixture/release.valid"
  printf '%s\n' '{"assets":[{"name":"SHA256SUMS","id":101},{"name":"SHA256SUMS","id":103}]}' > "$fixture/release.json"
  if run_seed > "$scratch/api-duplicate.stdout" 2> "$scratch/api-duplicate.stderr"; then
    echo 'ambiguous API assets unexpectedly passed selection' >&2
    exit 1
  fi
  grep -F 'expected one release asset' "$scratch/api-duplicate.stderr" >/dev/null
  test ! -e "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"
  cp "$fixture/release.valid" "$fixture/release.json"
  unset HARN_SEED_TEST_API_FALLBACK HARN_SEED_TEST_API_LOG
  echo 'Seed API fallback: exact assets, checksum and ambiguity controls passed.'
else
  echo 'API fallback tests skipped: jq is unavailable' >&2
fi

rm -rf "$scratch/cache-home"
run_seed >/dev/null &
first=$!
run_seed >/dev/null &
second=$!
wait "$first"
wait "$second"
cmp "$fixture/SHA256SUMS" "$cache/SHA256SUMS"
cmp "$fixture/harn-x86_64-unknown-linux-gnu.tar.xz" "$cache/harn-x86_64-unknown-linux-gnu.tar.xz"
