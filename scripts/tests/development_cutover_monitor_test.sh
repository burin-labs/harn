#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo_root/.github/workflows/development-cutover-monitor.yml"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/repo"
rows="$tmp_root/pr-rows.tsv"
mkdir -p "$fixture"

git -C "$fixture" init --quiet
git -C "$fixture" config user.name "Development Cutover Monitor Test"
git -C "$fixture" config user.email "development-cutover-monitor@example.com"
git -C "$fixture" config commit.gpgsign false
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

# An open exact-target PR makes the owed cutover visible and suppresses the
# alarm while remediation is already in flight.
git -C "$fixture" show HEAD~1:Cargo.toml > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'fixture bare main'
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
printf 'automation/development-1.2.4-dev\tOPEN\t-\thttps://example.invalid/pull/42\n' > "$rows"
open_log="$tmp_root/open.log"
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
  "$repo_root/scripts/check_development_cutover.sh" >"$open_log"
grep -Fq 'remediation_pr_count=1' "$open_log"
grep -Fq 'OPEN:https://example.invalid/pull/42' "$open_log"

printf 'automation/development-1.2.4-dev\tMERGED\t2026-08-29T23:00:00Z\thttps://example.invalid/pull/41\n' > "$rows"
merged_log="$tmp_root/merged.log"
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
  "$repo_root/scripts/check_development_cutover.sh" >"$merged_log"
grep -Fq 'remediation_pr_count=1' "$merged_log"
grep -Fq 'MERGED:https://example.invalid/pull/41' "$merged_log"

grep -Fq 'cron: "*/5 * * * *"' "$workflow"
grep -Fq 'run: ./scripts/check_development_cutover.sh' "$workflow"
grep -Fq 'context="development cutover"' "$workflow"

echo "development_cutover_monitor_test: ok"
