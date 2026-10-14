#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *workflows/ci.yml/runs*) cat "$FIXTURE/runs" ;;
  *runs/71/jobs*) cat "$FIXTURE/jobs" ;;
  *) exit 9 ;;
esac
SH
chmod +x "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH" FIXTURE="$scratch" GH_REPO=fixture/repo
export SOURCE_SHA=1111111111111111111111111111111111111111
printf '71\t%s\tcompleted\tsuccess\n' "$SOURCE_SHA" > "$scratch/runs"
names=('CI status' 'Verify publishable crates' 'Stack frame budget' 'Rust workspace tests' 'Run Linux sandbox tests' 'Windows cross-compile check' 'Rust on macOS (deny-warnings build + lint)')
for name in "${names[@]}"; do printf '%s\tcompleted\tsuccess\n' "$name"; done > "$scratch/jobs"
bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log"
grep -q 'Full-suite CI succeeded' "$scratch/log"
for name in "${names[@]}"; do
  cp "$scratch/jobs" "$scratch/good"
  awk -F '\t' -v name="$name" '$1 != name' "$scratch/good" > "$scratch/jobs"
  if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
    echo "Missing proof accepted: $name" >&2; exit 1
  fi
  mv "$scratch/good" "$scratch/jobs"
done
for outcome in failure cancelled ''; do
  printf '71\t%s\tcompleted\t%s\n' "$SOURCE_SHA" "$outcome" > "$scratch/runs"
  if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
    echo "Unqualified run accepted: $outcome" >&2; exit 1
  fi
done
: > "$scratch/runs"
if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
  echo 'No run was accepted as proof' >&2; exit 1
fi
printf '71\t%s\tcompleted\tsuccess\n' 2222222222222222222222222222222222222222 > "$scratch/runs"
if bash "$root/scripts/ci/require_full_suite.sh" > "$scratch/log" 2>&1; then
  echo 'Another commit was accepted as proof' >&2; exit 1
fi
echo 'Full-suite release gate: complete proof passes; missing families, missing runs, failures, and wrong source fail.'
