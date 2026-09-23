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
    jq_program=""
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "--jq" ]]; then
        jq_program="$2"
        shift 2
      else
        shift
      fi
    done
    jq -r "$jq_program" <<< "${FAKE_GH_PRS:-[]}"
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
  printf '%s\n' "$fixture"
}

add_fragments() {
  local fixture="$1"
  printf -- '- **Parser accepts trailing commas (#8557).**\n' > "$fixture/changelog.d/8557.added.md"
  printf -- '- **Retry budget is spent on retries (#8012).**\n' > "$fixture/changelog.d/8012.fixed.md"
  git -C "$fixture" add -A
  git -C "$fixture" commit --quiet -m "add fragments"
}

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
  if grep -Eq '^(make|gate|publish) |^gh pr create' "$case_record"; then
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

# --- main already at a stable version: the development bump has not landed ---
stable=$(new_fixture stable 1.2.4)
add_fragments "$stable"
run_opener "$stable"
[[ "$case_status" -eq 0 ]] || { cat "$case_output" >&2; fail "stable-version case failed"; }
grep -Fxq "action=none" "$case_outputs" || fail "stable-version case did not report action=none"
assert_no_side_effects "stable version" "$stable"
if grep -q '^gh ' "$case_record"; then
  fail "stable-version case queried GitHub before deciding there is nothing to release"
fi

echo "open_release_pr_test: ok"
