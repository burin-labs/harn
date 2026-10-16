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
tar -czf "$fixture/harn-x86_64-unknown-linux-gnu.tar.gz" -C "$fixture/archive" harn
if command -v sha256sum >/dev/null 2>&1; then
  checksum=$(sha256sum "$fixture/harn-x86_64-unknown-linux-gnu.tar.gz")
else
  checksum=$(shasum -a 256 "$fixture/harn-x86_64-unknown-linux-gnu.tar.gz")
fi
checksum=${checksum%% *}
printf '%s  %s\n' "$checksum" harn-x86_64-unknown-linux-gnu.tar.gz > "$fixture/SHA256SUMS"

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
cmp "$fixture/harn-x86_64-unknown-linux-gnu.tar.gz" "$cache/harn-x86_64-unknown-linux-gnu.tar.gz"

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
cp "$fixture/harn-x86_64-unknown-linux-gnu.tar.gz" "$fixture/archive.valid"
printf '%s\n' corrupt > "$fixture/harn-x86_64-unknown-linux-gnu.tar.gz"
"$real_rm" -f "$cache/harn-x86_64-unknown-linux-gnu.tar.gz"
if run_seed > "$scratch/corrupt.stdout" 2> "$scratch/corrupt.stderr"; then
  echo 'corrupt seed unexpectedly passed verification' >&2
  exit 1
fi
grep -F 'seed checksum mismatch' "$scratch/corrupt.stderr" >/dev/null
leftover=$(cat "$HARN_SEED_TEST_LEFTOVER")
grep -F "warning: bootstrap seed cleanup left $leftover" "$scratch/corrupt.stderr" >/dev/null
test ! -e "$cache/harn-x86_64-unknown-linux-gnu.tar.gz"
"$real_rm" -rf "$leftover"
cp "$fixture/archive.valid" "$fixture/harn-x86_64-unknown-linux-gnu.tar.gz"
"$real_rm" -f "$tools/rm"
run_seed >/dev/null

PATH="$tools:$PATH" \
XDG_CACHE_HOME="$scratch/cache-home" \
HARN_EXT_BOOTSTRAP_SEED_VERSION=9.8.7 \
HARN_BOOTSTRAP_OFFLINE=1 \
HARN_SEED_TEST_FIXTURE="$scratch/absent" \
  sh "$repo/scripts/bootstrap-harn.sh" --help >/dev/null

rm -f "$cache/harn-x86_64-unknown-linux-gnu.tar.gz"
if PATH="$tools:$PATH" XDG_CACHE_HOME="$scratch/cache-home" \
  HARN_EXT_BOOTSTRAP_SEED_VERSION=9.8.7 HARN_SEED_TEST_INTERRUPT=1 \
  HARN_SEED_TEST_FIXTURE="$fixture" sh "$repo/scripts/bootstrap-harn.sh" --help >/dev/null 2>&1; then
  echo 'interrupted seed download unexpectedly succeeded' >&2
  exit 1
fi
test ! -e "$cache/harn-x86_64-unknown-linux-gnu.tar.gz"

rm -rf "$scratch/cache-home"
run_seed >/dev/null &
first=$!
run_seed >/dev/null &
second=$!
wait "$first"
wait "$second"
cmp "$fixture/SHA256SUMS" "$cache/SHA256SUMS"
cmp "$fixture/harn-x86_64-unknown-linux-gnu.tar.gz" "$cache/harn-x86_64-unknown-linux-gnu.tar.gz"
