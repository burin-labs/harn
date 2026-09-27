#!/usr/bin/env bash
# Tests for scripts/open_release_pr.sh against fixture repositories.
#
# The opener's decision and the real `release_ship.sh --prepare` path run as
# they do in the workflow. The fold and release metadata run on a real Harn
# binary. GitHub (`gh`), the signed-commit publisher, the version-bump gate,
# and the Make targets that regenerate derived files are stubs that record
# what they were asked to do.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
real_harn="${HARN_RELEASE_METADATA_BIN:-${HARN_BIN:-}}"
if [[ -z "$real_harn" || ! -x "$real_harn" ]]; then
  echo "open_release_pr_test requires HARN_RELEASE_METADATA_BIN or HARN_BIN" >&2
  exit 1
fi

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT
bin_dir="$tmp_root/bin"
mkdir -p "$bin_dir"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# `gh` stub. `pr list` applies the caller's own --jq program to $FAKE_GH_PRS,
# so the title/branch match under test is the opener's real jq expression.
cat > "$bin_dir/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'gh %s\n' "$*" >> "$OPENER_RECORD"
case "$1 $2" in
  "pr list")
    if [[ "${FAKE_GH_LIST_FAIL:-0}" == 1 ]]; then
      echo "HTTP 502: Bad Gateway" >&2
      exit 1
    fi
    # FAKE_GH_PRS_LATER answers every lookup after the first, for a pull
    # request that opens while the run is preparing.
    lists=$(( $(cat "$OPENER_STATE/list-count" 2>/dev/null || echo 0) + 1 ))
    printf '%s\n' "$lists" > "$OPENER_STATE/list-count"
    prs="${FAKE_GH_PRS:-[]}"
    if (( lists > 1 )) && [[ -n "${FAKE_GH_PRS_LATER:-}" ]]; then
      prs="$FAKE_GH_PRS_LATER"
    fi
    jq_program=""
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "--jq" ]]; then
        jq_program="$2"
        shift 2
      else
        shift
      fi
    done
    jq -r "$jq_program" <<< "$prs"
    ;;
  "pr merge")
    if [[ "${FAKE_GH_MERGE_FAIL:-0}" == 1 ]]; then
      echo "GraphQL: Auto merge is not allowed for this repository" >&2
      exit 1
    fi
    ;;
  "pr create")
    cp "$HARN_RELEASE_ROOT/CHANGELOG.md" "$OPENER_STATE/changelog-at-create.md"
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "--body-file" ]]; then
        cp "$2" "$OPENER_STATE/body.md"
      fi
      shift
    done
    printf 'https://github.com/example/harn/pull/9001\n'
    ;;
  "pr edit")
    cp "$HARN_RELEASE_ROOT/CHANGELOG.md" "$OPENER_STATE/changelog-at-edit.md"
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "--body-file" ]]; then
        cp "$2" "$OPENER_STATE/body.md"
      fi
      shift
    done
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 2
    ;;
esac
SH

# Publisher stub: HARN_BIN is used only to publish the signed commit.
cat > "$bin_dir/harn-publisher" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  "run --no-sandbox "*"/publish_branch_commit.harn")
    {
      printf 'publish branch=%s\n' "$HARN_BRANCH_COMMIT_BRANCH"
      printf 'publish base=%s\n' "$HARN_BRANCH_COMMIT_BASE_OID"
      printf 'publish headline=%s\n' "$HARN_BRANCH_COMMIT_HEADLINE"
      printf 'publish token=%s\n' "$HARN_BRANCH_COMMIT_TOKEN"
    } >> "$OPENER_RECORD"
    git status --porcelain=v1 > "$OPENER_STATE/status-at-publish.txt"
    ;;
  *)
    echo "unexpected publisher invocation: $*" >&2
    exit 2
    ;;
esac
SH

cat > "$bin_dir/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'make %s\n' "$*" >> "$OPENER_RECORD"
SH

# Version-bump gate stub: rewrites the workspace version the way
# `release_gate.sh prepare` does, without building anything.
cat > "$bin_dir/release-gate" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'gate %s\n' "$*" >> "$OPENER_RECORD"
[[ "${1:-}" == prepare ]] || { echo "unexpected gate invocation: $*" >&2; exit 2; }
if [[ "${FAKE_GATE_FAIL:-0}" == 1 ]]; then
  echo "injected version-bump failure" >&2
  exit 7
fi
sed 's/^version = "1.2.4-dev"$/version = "1.2.4"/' Cargo.toml > Cargo.toml.next
mv Cargo.toml.next Cargo.toml
SH
chmod +x "$bin_dir/gh" "$bin_dir/harn-publisher" "$bin_dir/make" "$bin_dir/release-gate"

new_fixture() {
  local name="$1"
  local version="$2"
  local fixture="$tmp_root/$name"
  mkdir -p \
    "$fixture/.github" \
    "$fixture/crates/example" \
    "$fixture/changelog.d" \
    "$fixture/crates/harn-hostlib/data/grammar-fitness" \
    "$fixture/docs/src/spec/language" \
    "$fixture/docs/theme"
  printf '[workspace]\nversion = "%s"\nmembers = ["crates/example"]\nresolver = "2"\n' \
    "$version" > "$fixture/Cargo.toml"
  printf '[package]\nname = "example"\nversion = "1.2.3"\nedition = "2021"\n' \
    > "$fixture/crates/example/Cargo.toml"
  printf '# fake lock\n' > "$fixture/Cargo.lock"
  printf '{"schema_version":1,"releases":[]}\n' > "$fixture/.github/release-withdrawals.json"
  printf '# Changelog\n\n## v1.2.3\n\n- **Older fix (#10).**\n' > "$fixture/CHANGELOG.md"
  printf '# fragments\n' > "$fixture/changelog.d/README.md"
  : > "$fixture/changelog.d/.gitkeep"
  touch \
    "$fixture/crates/harn-hostlib/data/grammar-fitness/receipt.v1.json" \
    "$fixture/docs/src/language-spec.md" \
    "$fixture/docs/src/SUMMARY.md" \
    "$fixture/docs/theme/harn-keywords.js"
  git -C "$fixture" init --quiet -b main
  git -C "$fixture" config user.name "Release PR Opener Test"
  git -C "$fixture" config user.email "release-pr-opener-test@example.invalid"
  git -C "$fixture" config commit.gpgsign false
  git -C "$fixture" add -A
  git -C "$fixture" commit --quiet -m initial
  # The opener re-reads origin/main before publishing, so each fixture has a
  # real remote. Its HEAD names main explicitly: a bare repository otherwise
  # follows the host's init.defaultBranch, and a clone of it checks out nothing.
  git init --quiet --bare -b main "$fixture.origin.git"
  git -C "$fixture" remote add origin "$fixture.origin.git"
  git -C "$fixture" push --quiet origin HEAD:refs/heads/main
  printf '%s\n' "$fixture"
}

add_fragments() {
  local fixture="$1"
  printf -- '- **Parser accepts trailing commas (#8557).**\n' > "$fixture/changelog.d/8557.added.md"
  printf -- '- **Retry budget is spent on retries (#8012).**\n' > "$fixture/changelog.d/8012.fixed.md"
  git -C "$fixture" add -A
  git -C "$fixture" commit --quiet -m "add fragments"
  git -C "$fixture" push --quiet origin HEAD:refs/heads/main
}

# Land this version's release commit on origin/main from another clone, the way
# a release pull request merging during the run would. The fixture's own
# checkout keeps reading the development version.
merge_release_on_origin() {
  local fixture="$1"
  local other="$fixture.other"
  git clone --quiet "$fixture.origin.git" "$other"
  git -C "$other" config user.name "Release PR Opener Test"
  git -C "$other" config user.email "release-pr-opener-test@example.invalid"
  git -C "$other" config commit.gpgsign false
  sed 's/^version = "1.2.4-dev"$/version = "1.2.4"/' "$other/Cargo.toml" > "$other/Cargo.toml.next"
  mv "$other/Cargo.toml.next" "$other/Cargo.toml"
  git -C "$other" commit --quiet -am "Release v1.2.4"
  git -C "$other" push --quiet origin HEAD:refs/heads/main
}

# Publish release/v1.2.4 on origin as one commit on the fixture's current main,
# the shape the opener leaves: the fragments it folded are deleted.
open_release_branch_on_origin() {
  local fixture="$1"
  git -C "$fixture" switch --quiet -c release/v1.2.4
  git -C "$fixture" rm --quiet -- 'changelog.d/*.added.md' 'changelog.d/*.fixed.md'
  git -C "$fixture" commit --quiet -m "Release v1.2.4"
  git -C "$fixture" push --quiet origin HEAD:refs/heads/release/v1.2.4
  git -C "$fixture" switch --quiet main
  git -C "$fixture" branch --quiet -D release/v1.2.4
}

add_late_fragment() {
  local fixture="$1"
  printf -- '- **Late fix rides the release (#8900).**\n' > "$fixture/changelog.d/8900.fixed.md"
  git -C "$fixture" add -A
  git -C "$fixture" commit --quiet -m "late fragment"
  git -C "$fixture" push --quiet origin HEAD:refs/heads/main
}

open_release_pr='[{"url":"https://github.com/example/harn/pull/77","title":"Release v1.2.4","headRefName":"release/v1.2.4"}]'

# Run the opener in a fixture. Sets case_output, case_record, case_outputs,
# case_status, and case_state.
run_opener() {
  local fixture="$1"
  shift
  case_state="$tmp_root/state-$(basename "$fixture")"
  mkdir -p "$case_state"
  case_record="$case_state/record.txt"
  case_outputs="$case_state/github-output.txt"
  case_output="$case_state/output.txt"
  : > "$case_record"
  : > "$case_outputs"
  local opener_args=()
  if [[ -n "${OPENER_ARGS:-}" ]]; then
    opener_args=("$OPENER_ARGS")
  fi
  set +e
  env \
    HARN_RELEASE_ROOT="$fixture" \
    HARN_RELEASE_METADATA_BIN="$real_harn" \
    HARN_RELEASE_GATE_SCRIPT="$bin_dir/release-gate" \
    HARN_BIN="$bin_dir/harn-publisher" \
    GH_TOKEN=fixture-token \
    GITHUB_REPOSITORY=example/harn \
    GITHUB_OUTPUT="$case_outputs" \
    OPENER_RECORD="$case_record" \
    OPENER_STATE="$case_state" \
    PATH="$bin_dir:$PATH" \
    "$@" \
    "$repo_root/scripts/open_release_pr.sh" ${opener_args[@]+"${opener_args[@]}"} \
    > "$case_output" 2>&1
  case_status=$?
  set -e
}

assert_no_side_effects() {
  local label="$1"
  local fixture="$2"
  if grep -Eq '^(make|gate|publish) |^gh pr (create|merge)' "$case_record"; then
    cat "$case_record" >&2
    fail "$label: prepared, published, or opened a pull request"
  fi
  [[ "$(git -C "$fixture" branch --show-current)" == main ]] \
    || fail "$label: left main for $(git -C "$fixture" branch --show-current)"
  [[ -z "$(git -C "$fixture" status --porcelain)" ]] \
    || fail "$label: changed the checkout"
}

# --- Fragments on main and no release pull request: opens one ----------------
opens=$(new_fixture opens 1.2.4-dev)
add_fragments "$opens"
base_oid=$(git -C "$opens" rev-parse HEAD)
# An open release pull request for another version must not stop this one.
FAKE_GH_PRS='[{"url":"https://github.com/example/harn/pull/1","title":"Release v1.2.3","headRefName":"release/v1.2.3"}]' \
  run_opener "$opens"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "opener failed with fragments on main"; }
grep -Fxq "action=opened" "$case_outputs" || fail "opened case did not report action=opened"
grep -Fxq "version=1.2.4" "$case_outputs" || fail "opened case reported the wrong version"
grep -Fxq "pr_url=https://github.com/example/harn/pull/9001" "$case_outputs" \
  || fail "opened case did not report the pull request"
grep -Fq 'gh pr create --base main --head release/v1.2.4 --title Release v1.2.4 --body-file' "$case_record" \
  || { cat "$case_record" >&2; fail "pull request title or branch is not exactly Release v1.2.4 from release/v1.2.4"; }
grep -Fxq "publish headline=Release v1.2.4" "$case_record" || fail "signed commit headline is not Release v1.2.4"
grep -Fxq "publish branch=release/v1.2.4" "$case_record" || fail "signed commit went to the wrong branch"
grep -Fxq "publish base=$base_oid" "$case_record" || fail "signed commit is not based on main's head"
grep -Fxq "publish token=fixture-token" "$case_record" || fail "signed commit does not use the App token"
grep -Fxq 'version = "1.2.4"' "$opens/Cargo.toml" || fail "workspace version was not bumped to 1.2.4"
expected_changelog=$'# Changelog\n\n## v1.2.4\n\n### Added\n\n- **Parser accepts trailing commas (#8557).**\n\n### Fixed\n\n- **Retry budget is spent on retries (#8012).**\n\n## v1.2.3\n\n- **Older fix (#10).**'
[[ "$(cat "$case_state/changelog-at-create.md")" == "$expected_changelog" ]] || {
  diff <(printf '%s\n' "$expected_changelog") "$case_state/changelog-at-create.md" >&2 || true
  fail "the published CHANGELOG.md is not the folded release section"
}
for fragment in changelog.d/8557.added.md changelog.d/8012.fixed.md; do
  grep -Fxq "D  $fragment" "$case_state/status-at-publish.txt" \
    || { cat "$case_state/status-at-publish.txt" >&2; fail "published tree does not delete $fragment"; }
done
grep -Fxq "M  CHANGELOG.md" "$case_state/status-at-publish.txt" \
  || fail "published tree does not carry the folded CHANGELOG.md"
[[ -e "$opens/changelog.d/README.md" ]] || fail "fold deleted a non-fragment file"
grep -Fq "folds 2 changelog fragment(s)" "$case_state/body.md" || fail "pull request body does not count the folded fragments"
# Auto-merge is armed by the same run that opens the pull request, right after
# it opens and before its checks can settle, and it is the run's last GitHub
# call.
create_line=$(grep -n -m1 '^gh pr create ' "$case_record" | cut -d: -f1)
merge_line=$(grep -n -Fx 'gh pr merge https://github.com/example/harn/pull/9001 --auto --squash' "$case_record" | cut -d: -f1 || true)
[[ -n "$merge_line" ]] || { cat "$case_record" >&2; fail "opened pull request was not armed for auto-merge"; }
[[ "$merge_line" -gt "$create_line" ]] || fail "auto-merge was armed before the pull request existed"
[[ "$(grep '^gh ' "$case_record" | tail -n 1)" == "gh pr merge https://github.com/example/harn/pull/9001 --auto --squash" ]] \
  || { cat "$case_record" >&2; fail "arming is not the opening run's last GitHub call"; }

# --- A failed arm fails the run loudly and names the unarmed pull request ----
unarmed=$(new_fixture unarmed 1.2.4-dev)
add_fragments "$unarmed"
FAKE_GH_MERGE_FAIL=1 run_opener "$unarmed"
[[ "$case_status" -ne 0 ]] || fail "opener succeeded although arming auto-merge failed"
grep -Fq "could not arm auto-merge on https://github.com/example/harn/pull/9001" "$case_output" \
  || { cat "$case_output" >&2; fail "arming failure did not name the unarmed pull request"; }
grep -Fq 'gh pr merge https://github.com/example/harn/pull/9001 --auto --squash' "$case_record" \
  || fail "arming failure case never attempted to arm"
grep -Fxq "pr_url=https://github.com/example/harn/pull/9001" "$case_outputs" \
  || fail "arming failure did not report the opened pull request"
if grep -Fxq "action=opened" "$case_outputs"; then
  fail "arming failure still reported action=opened"
fi

# --- This version's release merges while the run prepares: no second PR -----
moved=$(new_fixture moved 1.2.4-dev)
add_fragments "$moved"
merge_release_on_origin "$moved"
run_opener "$moved"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "main-moved case failed"; }
grep -Fxq "action=none" "$case_outputs" || fail "main-moved case did not report action=none"
grep -Fq "main moved from 1.2.4-dev to 1.2.4 while this run prepared Release v1.2.4" "$case_output" \
  || { cat "$case_output" >&2; fail "main-moved case did not name the reason"; }
if grep -Eq '^publish |^gh pr (create|merge)' "$case_record"; then
  fail "opener published a release pull request for a version already on main"
fi

# --- A release PR opens while the run prepares: name it, publish nothing -----
raced=$(new_fixture raced 1.2.4-dev)
add_fragments "$raced"
FAKE_GH_PRS_LATER='[{"url":"https://github.com/example/harn/pull/80","title":"Release v1.2.4","headRefName":"release/v1.2.4"}]' \
  run_opener "$raced"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "raced case failed"; }
grep -Fxq "pr_url=https://github.com/example/harn/pull/80" "$case_outputs" \
  || fail "a release pull request opened during prepare was not named"
if grep -Eq '^publish |^gh pr (create|merge)' "$case_record"; then
  fail "opener published over a release pull request that opened during prepare"
fi

# --- A failure after the fold restores the notes and publishes nothing --------
rollback=$(new_fixture rollback 1.2.4-dev)
add_fragments "$rollback"
FAKE_GATE_FAIL=1 run_opener "$rollback"
[[ "$case_status" -ne 0 ]] || fail "opener succeeded after an injected prepare failure"
grep -Fq "injected version-bump failure" "$case_output" \
  || { cat "$case_output" >&2; fail "prepare failure was not reported"; }
if grep -Eq '^publish |^gh pr create' "$case_record"; then
  fail "opener published or opened a pull request after prepare failed"
fi
[[ -z "$(git -C "$rollback" status --porcelain)" ]] || {
  git -C "$rollback" status --porcelain >&2
  fail "failed prepare did not restore CHANGELOG.md and the fragments"
}

# --- --plan with fragments: decides, touches nothing -------------------------
planned=$(new_fixture planned 1.2.4-dev)
add_fragments "$planned"
OPENER_ARGS=--plan run_opener "$planned"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "--plan failed"; }
grep -Fxq "action=open" "$case_outputs" || fail "--plan did not decide to open"
assert_no_side_effects "--plan" "$planned"

# --- No fragments: no-op with a notice ----------------------------------------
empty=$(new_fixture empty 1.2.4-dev)
run_opener "$empty"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "opener failed without fragments"; }
grep -Fxq "action=none" "$case_outputs" || fail "no-fragment case did not report action=none"
grep -Fq "::notice title=Nothing to release::main has no unreleased changelog fragments" "$case_output" \
  || { cat "$case_output" >&2; fail "no-fragment case did not explain the no-op"; }
assert_no_side_effects "no fragments" "$empty"

# --- An open Release v1.2.4 pull request: stop and name it --------------------
by_title=$(new_fixture by-title 1.2.4-dev)
add_fragments "$by_title"
FAKE_GH_PRS='[{"url":"https://github.com/example/harn/pull/77","title":"Release v1.2.4","headRefName":"someone/else"}]' \
  run_opener "$by_title"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "existing-title case failed"; }
grep -Fxq "action=existing" "$case_outputs" || fail "existing-title case did not report action=existing"
grep -Fq "Release v1.2.4 is open: https://github.com/example/harn/pull/77" "$case_output" \
  || { cat "$case_output" >&2; fail "existing-title case did not name the open pull request"; }
assert_no_side_effects "existing title" "$by_title"

by_branch=$(new_fixture by-branch 1.2.4-dev)
add_fragments "$by_branch"
open_release_branch_on_origin "$by_branch"
FAKE_GH_PRS='[{"url":"https://github.com/example/harn/pull/78","title":"Retitled","headRefName":"release/v1.2.4"}]' \
  run_opener "$by_branch"
grep -Fxq "pr_url=https://github.com/example/harn/pull/78" "$case_outputs" \
  || fail "an open pull request from release/v1.2.4 did not stop the opener"
assert_no_side_effects "existing branch" "$by_branch"

# --- An unreadable pull request list refuses; it is not "none open" -----------
unproved=$(new_fixture unproved 1.2.4-dev)
add_fragments "$unproved"
FAKE_GH_LIST_FAIL=1 run_opener "$unproved"
[[ "$case_status" -ne 0 ]] || fail "opener proceeded when the pull request list failed"
grep -Fq "refusing to open Release v1.2.4 on unproved state" "$case_output" \
  || { cat "$case_output" >&2; fail "list failure did not explain the refusal"; }
assert_no_side_effects "list failure" "$unproved"

# --- main at a stable version with fragments pending: no-op, reason named ----
# This is main between a release commit and its post-publication development
# bump: promotion in flight, or a failed candidate build. Fragments that landed
# meanwhile wait for the next development version.
stable=$(new_fixture stable 1.2.4)
add_fragments "$stable"
run_opener "$stable"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "stable-version case failed"; }
grep -Fxq "action=none" "$case_outputs" || fail "stable-version case did not report action=none"
grep -Fq "::notice title=Nothing to release::main declares 1.2.4, not an X.Y.Z-dev development version. The next release starts once the development bump lands." "$case_output" \
  || { cat "$case_output" >&2; fail "stable-version case did not name the reason"; }
assert_no_side_effects "stable version" "$stable"
if grep -q '^gh ' "$case_record"; then
  fail "stable-version case queried GitHub before deciding there is nothing to release"
fi

# --- A fragment lands while the release PR is open: refold it in place --------
refold=$(new_fixture refold 1.2.4-dev)
add_fragments "$refold"
open_release_branch_on_origin "$refold"
add_late_fragment "$refold"
refold_base=$(git -C "$refold" rev-parse HEAD)
FAKE_GH_PRS="$open_release_pr" OPENER_ARGS=--refold-only run_opener "$refold"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "refold failed"; }
grep -Fxq "action=refolded" "$case_outputs" || { cat "$case_output" >&2; fail "refold did not report action=refolded"; }
grep -Fxq "pr_url=https://github.com/example/harn/pull/77" "$case_outputs" || fail "refold did not name the pull request"
grep -Fq "misses 1 fragment(s) now on main" "$case_output" || { cat "$case_output" >&2; fail "refold did not name the missing fragment"; }
grep -Fxq "publish branch=release/v1.2.4" "$case_record" || fail "refold published to the wrong branch"
grep -Fxq "publish base=$refold_base" "$case_record" || fail "refold is not based on main's new head"
if grep -q '^gh pr create' "$case_record"; then
  fail "refold opened a second pull request"
fi
grep -Fq 'gh pr edit https://github.com/example/harn/pull/77 --body-file' "$case_record" \
  || { cat "$case_record" >&2; fail "refold did not update the pull request body"; }
grep -Fq "folds 3 changelog fragment(s)" "$case_state/body.md" || fail "refold body does not count every fragment"
grep -Fq -- '- **Late fix rides the release (#8900).**' "$case_state/changelog-at-edit.md" \
  || fail "refold did not fold the late fragment"
grep -Fxq "D  changelog.d/8900.fixed.md" "$case_state/status-at-publish.txt" \
  || { cat "$case_state/status-at-publish.txt" >&2; fail "refolded tree does not delete the late fragment"; }
[[ "$(grep '^gh ' "$case_record" | tail -n 1)" == "gh pr merge https://github.com/example/harn/pull/77 --auto --squash" ]] \
  || { cat "$case_record" >&2; fail "refold did not keep the pull request armed"; }

# --- The plan names a due refold ---------------------------------------------
planned_refold=$(new_fixture planned-refold 1.2.4-dev)
add_fragments "$planned_refold"
open_release_branch_on_origin "$planned_refold"
add_late_fragment "$planned_refold"
FAKE_GH_PRS="$open_release_pr" OPENER_ARGS=--plan run_opener "$planned_refold"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "refold plan failed"; }
grep -Fxq "action=refold" "$case_outputs" || fail "plan did not decide to refold"
assert_no_side_effects "refold plan" "$planned_refold"

# --- A release PR that folds everything on main is left alone -----------------
current=$(new_fixture current 1.2.4-dev)
add_fragments "$current"
open_release_branch_on_origin "$current"
FAKE_GH_PRS="$open_release_pr" OPENER_ARGS=--refold-only run_opener "$current"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "current release PR case failed"; }
grep -Fxq "action=existing" "$case_outputs" || fail "a current release pull request was not left alone"
assert_no_side_effects "current release PR" "$current"

# --- An unreadable release branch refuses; it is not "nothing to refold" ------
lost_branch=$(new_fixture lost-branch 1.2.4-dev)
add_fragments "$lost_branch"
FAKE_GH_PRS="$open_release_pr" run_opener "$lost_branch"
[[ "$case_status" -ne 0 ]] || fail "opener proceeded without reading the release branch"
grep -Fq "refusing to refold Release v1.2.4 on unproved state" "$case_output" \
  || { cat "$case_output" >&2; fail "unreadable release branch did not explain the refusal"; }
assert_no_side_effects "unreadable release branch" "$lost_branch"

# --- A push never opens a release: refold-only with none open is a no-op ------
push_only=$(new_fixture push-only 1.2.4-dev)
add_fragments "$push_only"
OPENER_ARGS=--refold-only run_opener "$push_only"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "refold-only case failed"; }
grep -Fxq "action=none" "$case_outputs" || fail "refold-only opened or planned a release"
assert_no_side_effects "refold-only without a release PR" "$push_only"

echo "open_release_pr_test: ok"
