#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo_root/.github/workflows/repository-state-reconciliation.yml"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/repo"
rows="$tmp_root/pr-rows.tsv"
bin_dir="$tmp_root/bin"
mkdir -p "$fixture" "$bin_dir"

git -C "$fixture" init -b main --quiet
git -C "$fixture" config user.name "Development Cutover Monitor Test"
git -C "$fixture" config user.email "development-cutover-monitor@example.com"
git -C "$fixture" config commit.gpgsign false
git -C "$fixture" config tag.gpgSign false
git -C "$fixture" config maintenance.auto false
git -C "$fixture" config gc.auto 0
printf '[workspace.package]\nversion = "1.2.3"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'Release v1.2.3'
git -C "$fixture" tag v1.2.3
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
: > "$rows"

bare_log="$tmp_root/bare.log"
bare_output="$tmp_root/bare-output"
if HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
  HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
  GITHUB_OUTPUT="$bare_output" \
    "$repo_root/scripts/check_development_cutover.sh" >"$bare_log" 2>&1; then
  echo "bare released main without a remediation PR did not alarm" >&2
  exit 1
fi
grep -Fq 'main_version=1.2.3' "$bare_log"
grep -Fq 'latest_tag=v1.2.3' "$bare_log"
grep -Fq 'expected_branch=automation/development-1.2.4' "$bare_log"
grep -Fq 'matching_pr_count=0' "$bare_log"
grep -Fq 'remediation_pr_count=0' "$bare_log"
grep -Fq 'open_pr_count=0' "$bare_log"
grep -Fq 'merged_pr_count=0' "$bare_log"
grep -Fq 'main owes the next version' "$bare_log"
grep -Fxq 'repair_owed=true' "$bare_output"
grep -Fxq 'repair_version=1.2.4' "$bare_output"
grep -Fxq 'repair_published_tag=v1.2.3' "$bare_output"

# A main still on the development identity owes the same next stable version: this
# is how main reaches its first unpublished version.
printf '[workspace.package]\nversion = "1.2.4-dev"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'Start 1.2.4-dev development'
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
identity_output="$tmp_root/identity-output"
if HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
  HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
  GITHUB_OUTPUT="$identity_output" \
    "$repo_root/scripts/check_development_cutover.sh" >"$tmp_root/identity.log" 2>&1; then
  echo "a development identity on main passed as current" >&2
  exit 1
fi
grep -Fxq 'repair_owed=true' "$identity_output"
grep -Fxq 'repair_version=1.2.4' "$identity_output"
printf 'automation/development-1.2.4\tOPEN\t-\thttps://example.invalid/pull/40\n' > "$rows"
identity_open_output="$tmp_root/identity-open-output"
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
GITHUB_OUTPUT="$identity_open_output" \
  "$repo_root/scripts/check_development_cutover.sh" >"$tmp_root/identity-open.log"
grep -Fxq 'state=pending' "$identity_open_output"
: > "$rows"

# A main declaring the next version is settled, not awaiting anything: a known
# non-null negative control, so the same probe reports its version and stays green.
printf '[workspace.package]\nversion = "1.2.4"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'Start 1.2.4 development'
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
current_log="$tmp_root/current.log"
current_output="$tmp_root/current-output"
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
GITHUB_OUTPUT="$current_output" \
  "$repo_root/scripts/check_development_cutover.sh" >"$current_log"
grep -Fq 'main_version=1.2.4' "$current_log"
grep -Fq 'current: main 1.2.4 declares the next unpublished version after v1.2.3' "$current_log"
grep -Fxq 'state=success' "$current_output"
if grep -Fq 'repair_owed=true' "$current_output"; then
  echo "settled main admitted automatic repair" >&2
  exit 1
fi

# Publishing that exact version changes the same observation into a real debt.
git -C "$fixture" tag v1.2.4
if HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
  HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
    "$repo_root/scripts/check_development_cutover.sh" >"$tmp_root/published.log" 2>&1; then
  echo "published stable main without a version PR did not alarm" >&2
  exit 1
fi
grep -Fq 'expected_next_version=1.2.5' "$tmp_root/published.log"
git -C "$fixture" tag -d v1.2.4 >/dev/null

# A retired, never-published version leaves main's stable version more than one
# patch ahead of the latest tag. That is still settled, not owed.
printf '[workspace.package]\nversion = "1.2.6"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'Start 1.2.6 after retired 1.2.5'
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
git -C "$fixture" tag v1.2.4
skipped_log="$tmp_root/skipped.log"
skipped_output="$tmp_root/skipped-output"
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
GITHUB_OUTPUT="$skipped_output" \
  "$repo_root/scripts/check_development_cutover.sh" >"$skipped_log" 2>&1 || {
  cat "$skipped_log" >&2
  exit 1
}
grep -Fxq 'state=success' "$skipped_output"
grep -Fq 'current: main 1.2.6 declares the next unpublished version after v1.2.4' "$skipped_log"
git -C "$fixture" tag -d v1.2.4 >/dev/null

# Any other version is unhealthy. Merely differing from the latest tag must
# not collapse an older stable or a skipped development identity into green.
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
  grep -Fq 'expected_next_version=1.2.4' "$wrong_log"
done

# An open exact-target PR is unfinished work, never proof that main converged.
printf '[workspace.package]\nversion = "1.2.3"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'fixture bare main'
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
printf 'automation/development-1.2.4\tOPEN\t-\thttps://example.invalid/pull/42\n' > "$rows"
open_log="$tmp_root/open.log"
open_output="$tmp_root/open-output"
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
GITHUB_OUTPUT="$open_output" \
  "$repo_root/scripts/check_development_cutover.sh" >"$open_log"
grep -Fq 'remediation_pr_count=1' "$open_log"
grep -Fq 'open_pr_count=1' "$open_log"
grep -Fq 'OPEN:https://example.invalid/pull/42' "$open_log"
grep -Fxq 'state=pending' "$open_output"
if grep -Fq 'repair_owed=true' "$open_output"; then
  echo "existing update admitted a competing automatic repair" >&2
  exit 1
fi

# An open update cannot excuse an unrelated version or duplicate custody.
printf '[workspace.package]\nversion = "1.2.2"\n' > "$fixture/Cargo.toml"
git -C "$fixture" add Cargo.toml
git -C "$fixture" commit --quiet -m 'Fixture stale main with open update'
git -C "$fixture" update-ref refs/remotes/origin/main HEAD
if HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
  HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
    "$repo_root/scripts/check_development_cutover.sh" >"$tmp_root/wrong-open.log" 2>&1; then
  echo "open update suppressed wrong main version" >&2
  exit 1
fi
grep -Fq 'wrong version: main 1.2.2' "$tmp_root/wrong-open.log"
printf 'automation/development-1.2.4\tOPEN\t-\thttps://example.invalid/pull/43\n' >> "$rows"
set +e
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
  "$repo_root/scripts/check_development_cutover.sh" >"$tmp_root/ambiguous.log" 2>&1
ambiguous_rc=$?
set -e
[[ "$ambiguous_rc" -eq 2 ]]
grep -Fq 'matching=2, open=2' "$tmp_root/ambiguous.log"

# A full query page cannot distinguish exactly 100 from truncated history.
: > "$rows"
for pr_number in $(seq 1 100); do
  printf 'automation/development-1.2.4\tCLOSED\t-\thttps://example.invalid/pull/%s\n' "$pr_number" >> "$rows"
done
set +e
HARN_DEVELOPMENT_CUTOVER_ROOT="$fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$rows" \
  "$repo_root/scripts/check_development_cutover.sh" >"$tmp_root/truncated.log" 2>&1
truncated_rc=$?
set -e
[[ "$truncated_rc" -eq 2 ]]
grep -Fq 'matching=100, open=0' "$tmp_root/truncated.log"

printf 'automation/development-1.2.4\tMERGED\t2026-08-29T23:00:00Z\thttps://example.invalid/pull/41\n' > "$rows"
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

# A certified release tag may name its immutable candidate rather than a main
# ancestor. The repository's stable tag list still owns publication identity,
# so a main already at the following version is current.
off_ancestry_fixture="$tmp_root/off-ancestry"
off_ancestry_rows="$tmp_root/off-ancestry-rows.tsv"
mkdir -p "$off_ancestry_fixture"
git -C "$off_ancestry_fixture" init -b main --quiet
git -C "$off_ancestry_fixture" config user.name "Development Cutover Monitor Test"
git -C "$off_ancestry_fixture" config user.email "development-cutover-monitor@example.com"
git -C "$off_ancestry_fixture" config commit.gpgsign false
git -C "$off_ancestry_fixture" config tag.gpgSign false
git -C "$off_ancestry_fixture" config maintenance.auto false
git -C "$off_ancestry_fixture" config gc.auto 0
printf '[workspace.package]\nversion = "1.2.3"\n' > "$off_ancestry_fixture/Cargo.toml"
git -C "$off_ancestry_fixture" add Cargo.toml
git -C "$off_ancestry_fixture" commit --quiet -m initial
git -C "$off_ancestry_fixture" tag v1.2.3
default_branch="$(git -C "$off_ancestry_fixture" branch --show-current)"
git -C "$off_ancestry_fixture" switch --quiet -c release-candidate
printf '[workspace.package]\nversion = "1.2.4"\n' > "$off_ancestry_fixture/Cargo.toml"
git -C "$off_ancestry_fixture" add Cargo.toml
git -C "$off_ancestry_fixture" commit --quiet -m 'Release v1.2.4 candidate'
git -C "$off_ancestry_fixture" tag v1.2.4
git -C "$off_ancestry_fixture" switch --quiet "$default_branch"
printf '[workspace.package]\nversion = "1.2.5"\n' > "$off_ancestry_fixture/Cargo.toml"
git -C "$off_ancestry_fixture" add Cargo.toml
git -C "$off_ancestry_fixture" commit --quiet -m 'Start 1.2.5 development'
git -C "$off_ancestry_fixture" update-ref refs/remotes/origin/main HEAD
: > "$off_ancestry_rows"

off_ancestry_log="$tmp_root/off-ancestry.log"
HARN_DEVELOPMENT_CUTOVER_ROOT="$off_ancestry_fixture" \
HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$off_ancestry_rows" \
  "$repo_root/scripts/check_development_cutover.sh" > "$off_ancestry_log"
grep -Fq 'latest_tag=v1.2.4' "$off_ancestry_log"
grep -Fq 'main_version=1.2.5' "$off_ancestry_log"

# The same non-ancestor release tag must reject a genuinely stale version rather
# than treating any development identity as current.
printf '[workspace.package]\nversion = "1.2.4-dev"\n' > "$off_ancestry_fixture/Cargo.toml"
git -C "$off_ancestry_fixture" add Cargo.toml
git -C "$off_ancestry_fixture" commit --quiet -m 'Fixture stale development fold'
git -C "$off_ancestry_fixture" update-ref refs/remotes/origin/main HEAD
stale_fold_log="$tmp_root/stale-fold.log"
if HARN_DEVELOPMENT_CUTOVER_ROOT="$off_ancestry_fixture" \
  HARN_DEVELOPMENT_CUTOVER_PR_ROWS_FILE="$off_ancestry_rows" \
    "$repo_root/scripts/check_development_cutover.sh" > "$stale_fold_log" 2>&1; then
  echo "stale development fold passed against a non-ancestor release tag" >&2
  exit 1
fi
grep -Fq 'latest_tag=v1.2.4' "$stale_fold_log"
grep -Fq 'main_version=1.2.4-dev' "$stale_fold_log"
grep -Fq 'expected_next_version=1.2.5' "$stale_fold_log"

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
grep -Fq 'run: ./scripts/publish_development_cutover_status.sh' "$workflow"

# Exercise the workflow's actual status publisher. Missing measurement must
# remain failed, and a measured pending phase must never turn into success.
cat > "$bin_dir/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$CUTOVER_TEST_PUBLISH_ARGS"
EOF
chmod +x "$bin_dir/gh"
for case_row in success:success:success success:pending:pending success:missing:failure success:invalid:failure failure:success:failure; do
  IFS=: read -r outcome measured expected <<< "$case_row"
  set +e
  (
    cd "$fixture"
    PATH="$bin_dir:$PATH" CUTOVER_TEST_PUBLISH_ARGS="$tmp_root/publish-$measured" \
    CHECK_OUTCOME="$outcome" MEASURED_STATE="${measured/missing/}" \
    GH_REPO=fixture/project TARGET_URL=https://example.invalid/run/1 \
      "$repo_root/scripts/publish_development_cutover_status.sh"
  ) > "$tmp_root/publish-$measured.log" 2>&1
  published_rc=$?
  set -e
  grep -Fxq "state=$expected" "$tmp_root/publish-$measured"
  if [[ "$expected" == failure ]]; then
    [[ "$published_rc" -eq 1 ]]
  else
    [[ "$published_rc" -eq 0 ]]
  fi
done
grep -Fq 'name: Cancel obsolete speculative workflows' "$workflow"
grep -Fq "if: github.event_name == 'merge_group'" "$workflow"
grep -Fq "github.event_name == 'merge_group' && 'merge_group' || 'cutover'" "$workflow"
grep -Fq 'run: ./scripts/cancel_superseded_merge_groups.sh --repo "$TARGET_REPO" --apply' "$workflow"

echo "development_cutover_monitor_test: ok"
