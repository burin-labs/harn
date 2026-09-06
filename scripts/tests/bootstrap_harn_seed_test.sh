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
exit 0
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
