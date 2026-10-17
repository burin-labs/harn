#!/usr/bin/env bash
# A main push reuses its merge group's CLI only when GitHub names a successful
# merge-group run of the same commit holding an unexpired bundle. Every other
# answer, including an unreadable one, must build.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FIXTURE/calls"
case "$*" in
  *'/runs?event=merge_group&head_sha='*)
    [[ "${RUNS:-ok}" != fail ]] || exit 1
    [[ "${RUNS:-ok}" != none ]] || exit 0
    echo 77
    ;;
  *'runs/77/artifacts?name=harn-cli.tar.zst'*)
    [[ "${ARTIFACT:-ok}" != fail ]] || exit 1
    [[ "${ARTIFACT:-ok}" != expired ]] || exit 0
    echo 901
    ;;
  *) echo "Unexpected gh call: $*" >&2; exit 9 ;;
esac
SH
chmod +x "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH" FIXTURE="$scratch" GH_REPO=fixture/repo
sha=$(printf 'a%.0s' {1..40})
run() { : > "$scratch/calls"; bash "$root/scripts/ci/find_merge_group_cli.sh" "$sha" 2>/dev/null; }

[[ "$(run)" == $'found=true\nrun_id=77' ]] || { echo 'a proven commit did not reuse its CLI' >&2; exit 1; }
grep -Fq "head_sha=$sha&status=success" "$scratch/calls" || { echo 'lookup was not bound to the commit and success' >&2; exit 1; }
for state in RUNS=none RUNS=fail ARTIFACT=expired ARTIFACT=fail; do
  out=$(env "$state" bash "$root/scripts/ci/find_merge_group_cli.sh" "$sha" 2>/dev/null)
  [[ "$out" == found=false ]] || { echo "$state reused a CLI: $out" >&2; exit 1; }
done
if bash "$root/scripts/ci/find_merge_group_cli.sh" abc123 >/dev/null 2>&1; then
  echo 'an abbreviated commit was accepted' >&2; exit 1
fi
echo 'find_merge_group_cli: reuse only for a proven commit with a live bundle; every other answer builds.'
