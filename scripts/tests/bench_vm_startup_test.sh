#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
fixtures="$tmp_root/fixtures"
record="$tmp_root/record.txt"
cache_root="$tmp_root/cache"
helper_cache="$tmp_root/helper-cache"
mkdir -p "$fake_bin" "$fixtures" "$cache_root" "$helper_cache"

cat > "$fake_bin/python3" <<'SH'
#!/usr/bin/env bash
echo "python3 must not run" >&2
exit 64
SH
chmod +x "$fake_bin/python3"

cat > "$fixtures/example.harn" <<'HARN'
pipeline main(task) {
  return nil
}
HARN

cat > "$fake_bin/harn-under-test" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'args=%s\n' "$*" >> "$BENCH_VM_RECORD"
printf 'cache=%s\n' "${HARN_CACHE_DIR-__unset__}" >> "$BENCH_VM_RECORD"
case "$*" in
  run\ *)
    exit 0
    ;;
  *)
    echo "unexpected fake harn invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_bin/harn-under-test"

cat > "$fake_bin/helper-harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" != "run" ]]; then
  echo "unexpected helper harn command: $*" >&2
  exit 2
fi
shift
helper_script="$1"
shift
if [[ "$1" != "--" ]]; then
  echo "helper invocation missing -- separator: $*" >&2
  exit 2
fi
shift
harn_bin=""
fixture=""
runs=""
cache_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --harn-bin)
      harn_bin="$2"
      shift 2
      ;;
    --fixture)
      fixture="$2"
      shift 2
      ;;
    --runs)
      runs="$2"
      shift 2
      ;;
    --cache-dir)
      cache_dir="$2"
      shift 2
      ;;
    *)
      echo "unexpected helper arg: $1" >&2
      exit 2
      ;;
  esac
done
printf 'helper=%s\n' "$helper_script" >> "$BENCH_VM_RECORD"
for ((i = 0; i < runs; i++)); do
  HARN_CACHE_DIR="$cache_dir" "$harn_bin" run "$fixture"
done
printf '1.000 1.000 1.000\n'
SH
chmod +x "$fake_bin/helper-harn"

PATH="$fake_bin:$PATH" \
  HARN_BIN="$fake_bin/harn-under-test" \
  HARN_BENCH_FIXTURES_DIR="$fixtures" \
  HARN_BENCH_CACHE_DIR="$cache_root" \
  HARN_BENCH_HELPER_CACHE_DIR="$helper_cache" \
  HARN_BENCH_HELPER_BIN="$fake_bin/helper-harn" \
  BENCH_VM_RECORD="$record" \
  "$repo_root/scripts/bench_vm.sh" --no-build --cold-start --startup-runs 2 > "$tmp_root/cold.txt"

if ! grep -Eq '^benchmark\[cold-start\][[:space:]]+runs[[:space:]]+min_ms[[:space:]]+avg_ms[[:space:]]+max_ms$' "$tmp_root/cold.txt"; then
  echo "cold-start output missing benchmark header" >&2
  cat "$tmp_root/cold.txt" >&2
  exit 1
fi
if [[ "$(grep -c '^args=run ' "$record")" -ne 2 ]]; then
  echo "cold-start mode did not run the fixture exactly twice" >&2
  cat "$record" >&2
  exit 1
fi
if grep -Fq "cache=$helper_cache" "$record"; then
  echo "benchmarked child inherited helper cache instead of benchmark cache" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
PATH="$fake_bin:$PATH" \
  HARN_BIN="$fake_bin/harn-under-test" \
  HARN_BENCH_FIXTURES_DIR="$fixtures" \
  HARN_BENCH_CACHE_DIR="$cache_root" \
  HARN_BENCH_HELPER_CACHE_DIR="$helper_cache" \
  HARN_BENCH_HELPER_BIN="$fake_bin/helper-harn" \
  BENCH_VM_RECORD="$record" \
  "$repo_root/scripts/bench_vm.sh" --no-build --warm-start --startup-runs 3 > "$tmp_root/warm.txt"

if ! grep -Eq '^benchmark\[warm-start\][[:space:]]+runs[[:space:]]+min_ms[[:space:]]+avg_ms[[:space:]]+max_ms$' "$tmp_root/warm.txt"; then
  echo "warm-start output missing benchmark header" >&2
  cat "$tmp_root/warm.txt" >&2
  exit 1
fi
if [[ "$(grep -c '^args=run ' "$record")" -ne 4 ]]; then
  echo "warm-start mode should run once for warmup plus three measured runs" >&2
  cat "$record" >&2
  exit 1
fi

echo "bench_vm_startup_test: ok"
