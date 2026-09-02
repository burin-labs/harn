#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo_root/.github/workflows/development-cutover-monitor.yml"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/repo"
rows="$tmp_root/pr-rows.tsv"
bin_dir="$tmp_root/bin"
mkdir -p "$fixture" "$bin_dir"

git -C "$fixture" init --quiet
git -C "$fixture" config user.name "Development Cutover Monitor Test"
git -C "$fixture" config user.email "development-cutover-monitor@example.com"
git -C "$fixture" config commit.gpgsign false
git -C "$fixture" config tag.gpgSign false
printf '[workspace.package]\nversion = "1.2.3"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'Release v1.2.3'
git -C "$fixture" tag v1.2.3
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
: > "$rows"

bare_log="$tmp_root/bare.log"
if HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
  HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
    "$repo_root/scripts/check_development_cutover.sh" >"$bare_log" 2>&1; then
  echo "bare released main without a remediation PR did not alarm" >&2
  exit 1
fi
grep -Fq 'main_version=1.2.3' "$bare_log"
grep -Fq 'latest_tag=v1.2.3' "$bare_log"
grep -Fq 'expected_branch=automation/development-1.2.4-dev' "$bare_log"
grep -Fq 'matching_pr_count=0' "$bare_log"
grep -Fq 'remediation_pr_count=0' "$bare_log"
grep -Fq 'open_pr_count=0' "$bare_log"
grep -Fq 'merged_pr_count=0' "$bare_log"
grep -Fq 'development cutover is owed' "$bare_log"

# A correctly cut-over main is a known non-null negative control: the same
# probe must report its measured version and remain silent.
printf '[workspace.package]\nversion = "1.2.4-dev"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'Start 1.2.4-dev development'
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
current_log="$tmp_root/current.log"
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
  "$repo_root/scripts/check_development_cutover.sh" >"$current_log"
grep -Fq 'main_version=1.2.4-dev' "$current_log"
grep -Fq 'matching_pr_count=0' "$current_log"
grep -Fq 'development cutover monitor: current:' "$current_log"

# Any other version is unhealthy. Merely differing from the latest tag must
# not collapse an older stable or a skipped development target into green.
for wrong_version in 1.2.2 1.2.5-dev; do
  printf '[workspace.package]\nversion = "%s"\n' "$wrong_version" > "$fixture/Cargo.toml"
  git -C "$fixture" add Cargo.toml
  git -C "$fixture" commit --quiet -m "fixture wrong main $wrong_version"
  git -C "$fixture" update-ref refs/remotes/origin/main HEAD
  wrong_log="$tmp_root/wrong-$wrong_version.log"
  if HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
    HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
      "$repo_root/scripts/check_development_cutover.sh" >"$wrong_log" 2>&1; then
    echo "wrong main version $wrong_version passed as current" >&2
    exit 1
  fi
  grep -Fq "main_version=$wrong_version" "$wrong_log"
  grep -Fq 'expected_development_version=1.2.4-dev' "$wrong_log"
done

# An open exact-target PR makes the owed cutover visible and suppresses the
# alarm while remediation is already in flight.
printf '[workspace.package]\nversion = "1.2.3"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'fixture bare main'
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
printf 'automation/development-1.2.4-dev\tOPEN\t-\thttps://example.invalid/pull/42\n' > "$rows"
open_log="$tmp_root/open.log"
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
  "$repo_root/scripts/check_development_cutover.sh" >"$open_log"
grep -Fq 'remediation_pr_count=1' "$open_log"
grep -Fq 'open_pr_count=1' "$open_log"
grep -Fq 'OPEN:https://example.invalid/pull/42' "$open_log"

printf 'automation/development-1.2.4-dev\tMERGED\t2026-08-29T23:00:00Z\thttps://example.invalid/pull/41\n' > "$rows"
merged_log="$tmp_root/merged.log"
if HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
  HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
    "$repo_root/scripts/check_development_cutover.sh" >"$merged_log" 2>&1; then
  echo "merged PR suppressed a wrong-main cutover alarm" >&2
  exit 1
fi
grep -Fq 'remediation_pr_count=1' "$merged_log"
grep -Fq 'open_pr_count=0' "$merged_log"
grep -Fq 'merged_pr_count=1' "$merged_log"
grep -Fq 'MERGED:https://example.invalid/pull/41' "$merged_log"

# An unreadable PR census is unknown, not a measured zero or green state.
cat > "$bin_dir/gh" <<'EOF'
#!/usr/bin/env bash
exit 17
EOF
chmod +x "$bin_dir/gh"
query_log="$tmp_root/query-failure.log"
set +e
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
PATH="$bin_dir:$PATH" \
  "$repo_root/scripts/check_development_cutover.sh" >"$query_log" 2>&1
query_rc=$?
set -e
[[ "$query_rc" -eq 2 ]] || {
  echo "PR query failure returned $query_rc instead of unknown rc 2" >&2
  exit 1
}
grep -Fq 'could not measure pull requests' "$query_log"

grep -Fq 'cron: "*/5 * * * *"' "$workflow"
grep -Fq 'run: ./scripts/check_development_cutover.sh' "$workflow"
grep -Fq 'context="development cutover"' "$workflow"

echo "development_cutover_monitor_test: ok"
