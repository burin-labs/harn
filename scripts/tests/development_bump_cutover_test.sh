#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/workspace"
bin_dir="$tmp_root/bin"
mkdir -p "$fixture/crates/example" "$bin_dir"

cat > "$fixture/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/example"]
[workspace.package]
version = "1.2.3"
EOF
cat > "$fixture/crates/example/Cargo.toml" <<'EOF'
[package]
name = "example"
version.workspace = true
EOF
printf '# initial lock\n' > "$fixture/Cargo.lock"
git -C "$fixture" init -b main --quiet
git -C "$fixture" config user.name "Development Cutover Test"
git -C "$fixture" config user.email "development-cutover-test@example.com"
git -C "$fixture" config commit.gpgsign false
git -C "$fixture" add .
git -C "$fixture" commit --quiet -m initial
git -C "$fixture" tag v1.2.3

# The opener re-reads origin/main before it opens anything, so the fixture needs
# a real remote rather than a detached working copy.
origin="$tmp_root/origin.git"
git init -b main --quiet --bare "$origin"
git -C "$fixture" remote add origin "$origin"
git -C "$fixture" push --quiet origin HEAD:refs/heads/main
git -C "$fixture" fetch --quiet origin main

cat > "$bin_dir/harn" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'harn\t%s\n' "$*" >> "$CUTOVER_RECORD"
case "$*" in
  *"/release_metadata.harn -- current "*)
    sed -n 's/^version = "\([^"]*\)"/\1/p' "$HARN_RELEASE_ROOT/Cargo.toml"
    ;;
  *"/release_metadata.harn -- development-target "*) printf '1.2.4-dev\n' ;;
  *"/release_metadata.harn -- develop "*)
    sed 's/version = "1.2.3"/version = "1.2.4-dev"/' \
      "$HARN_RELEASE_ROOT/Cargo.toml" > "$HARN_RELEASE_ROOT/Cargo.toml.next"
    mv "$HARN_RELEASE_ROOT/Cargo.toml.next" "$HARN_RELEASE_ROOT/Cargo.toml"
    ;;
  *"/sync_protocol_fixture_runtime_versions.harn "*) ;;
  *"/sync_grammar_fitness_receipt.harn") ;;
  "dump-protocol-artifacts") ;;
  "run --no-sandbox "*"/publish_branch_commit.harn") ;;
  *) echo "unexpected fake Harn invocation: $*" >&2; exit 2 ;;
esac
EOF

cat > "$bin_dir/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  "metadata --format-version=1") printf '# reconciled\n' >> Cargo.lock ;;
  "metadata --format-version=1 --locked") grep -Fq '# reconciled' Cargo.lock ;;
  *) exit 2 ;;
esac
EOF

cat > "$bin_dir/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'gh\t%s\n' "$*" >> "$CUTOVER_RECORD"
case "$1 $2" in
  "release view")
    [[ "${CUTOVER_RELEASE_LOOKUP_FAIL:-0}" != 1 ]] || exit 1
    cat "$CUTOVER_PUBLICATION_FILE"
    ;;
  "pr list") printf '%s' "${CUTOVER_EXISTING_PR:-}" ;;
  "pr create") printf 'https://example.invalid/pull/42\n' ;;
  "pr edit"|"pr merge") ;;
  *) echo "unexpected fake gh invocation: $*" >&2; exit 2 ;;
esac
EOF

cat > "$bin_dir/make" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'make\t%s\n' "$*" >> "$CUTOVER_RECORD"
if [[ "${FAIL_GRAMMAR_CORPUS:-false}" == true ]]; then
  echo "forced stale grammar receipt" >&2
  exit 23
fi
EOF
chmod +x "$bin_dir/harn" "$bin_dir/cargo" "$bin_dir/gh" "$bin_dir/make"

# Execute the reusable/manual workflow's real planning command before reaching
# the opener. Only actual stable publication can authorize a development bump.
workflow="$repo_root/.github/workflows/open-development-bump.yml"
plan_command=$(awk '/        id: development$/{found=1} found && /        run: /{sub(/^        run: /, ""); print; exit}' "$workflow")
[[ -n "$plan_command" ]] || { echo "missing dispatchable cutover plan" >&2; exit 1; }
publication="$tmp_root/publication.json"
printf '%s\n' '{"tagName":"v1.2.3","isDraft":false,"isPrerelease":false,"publishedAt":"2026-10-03T00:00:00Z"}' > "$publication"
run_plan() {
  local name="$1"
  shift
  plan_outputs="$tmp_root/$name.outputs"
  plan_record="$tmp_root/$name.record"
  : > "$plan_outputs"
  set +e
  env CUTOVER_RECORD="$plan_record" CUTOVER_PUBLICATION_FILE="$publication" \
    HARN_RELEASE_ROOT="$fixture" GITHUB_REPOSITORY=example/harn \
    PUBLISHED_TAG=v1.2.3 GITHUB_OUTPUT="$plan_outputs" PATH="$bin_dir:$PATH" \
    "$@" bash -c "cd \"$repo_root\"; $plan_command" > "$tmp_root/$name.log" 2>&1
  plan_status=$?
  set -e
}
run_plan promoted
[[ "$plan_status" == 0 ]] || { cat "$tmp_root/promoted.log" >&2; exit 1; }
grep -Fxq 'required=true' "$plan_outputs"
grep -Fxq 'version=1.2.4-dev' "$plan_outputs"
grep -Fxq 'published_tag=v1.2.3' "$plan_outputs"
run_plan manual-latest PUBLISHED_TAG=
[[ "$plan_status" == 0 ]] || { cat "$tmp_root/manual-latest.log" >&2; exit 1; }
grep -Fxq 'required=true' "$plan_outputs"
grep -Fxq 'published_tag=v1.2.3' "$plan_outputs"
run_plan unreadable CUTOVER_RELEASE_LOOKUP_FAIL=1
[[ "$plan_status" != 0 && ! -s "$plan_outputs" ]] || { echo "unreadable publication authorized cutover" >&2; exit 1; }

for invalid in \
  '{}' \
  '{"tagName":"v1.2.3","isDraft":true,"isPrerelease":false,"publishedAt":"now"}' \
  '{"tagName":"v1.2.3","isDraft":false,"isPrerelease":true,"publishedAt":"now"}' \
  '{"tagName":"v1.2.3","isDraft":false,"isPrerelease":false,"publishedAt":null}' \
  '{"tagName":"v1.2.3","isDraft":false,"publishedAt":"now"}' \
  '{"tagName":"v1.2.2","isDraft":false,"isPrerelease":false,"publishedAt":"now"}' \
  'not-json'; do
  printf '%s\n' "$invalid" > "$publication"
  run_plan invalid-publication
  [[ "$plan_status" != 0 && ! -s "$plan_outputs" ]] \
    || { echo "unproved publication authorized cutover: $invalid" >&2; exit 1; }
done
printf '%s\n' '{"tagName":"v1.2.3","isDraft":false,"isPrerelease":false,"publishedAt":"2026-10-03T00:00:00Z"}' > "$publication"
run_plan invalid-tag PUBLISHED_TAG=v1.2.3-rc.1
[[ "$plan_status" != 0 && ! -s "$plan_outputs" ]] || { echo "prerelease authorized cutover" >&2; exit 1; }

# Feed the proved plan's exact identity through the existing owning opener.
run_plan proved
expected_version=$(sed -n 's/^version=//p' "$plan_outputs")
published_tag=$(sed -n 's/^published_tag=//p' "$plan_outputs")

record="$tmp_root/cutover.record"
outputs="$tmp_root/github.outputs"
CUTOVER_RECORD="$record" \
HARN_RELEASE_ROOT="$fixture" \
HARN_BIN="$bin_dir/harn" \
EXPECTED_DEVELOPMENT_VERSION="$expected_version" \
RELEASE_PUBLISHED_VERSION="$published_tag" \
GH_TOKEN=fixture-token \
GITHUB_OUTPUT="$outputs" \
PATH="$bin_dir:$PATH" \
  "$repo_root/scripts/open_development_bump.sh"

grep -Fq 'version = "1.2.4-dev"' "$fixture/Cargo.toml"
grep -Fq $'gh\tpr create ' "$record"
grep -Fq 'pr_url=https://example.invalid/pull/42' "$outputs"
grep -Fq 'skipped=false' "$outputs"

# A manual repair reuses a still-open cutover instead of replacing its branch.
open_fixture="$tmp_root/open-workspace"
cp -R "$fixture" "$open_fixture"
git -C "$open_fixture" checkout --quiet -- Cargo.toml Cargo.lock
open_record="$tmp_root/open.record"
open_outputs="$tmp_root/open.outputs"
CUTOVER_RECORD="$open_record" CUTOVER_EXISTING_PR=https://example.invalid/pull/42 \
HARN_RELEASE_ROOT="$open_fixture" HARN_BIN="$bin_dir/harn" \
EXPECTED_DEVELOPMENT_VERSION="$expected_version" RELEASE_PUBLISHED_VERSION="$published_tag" \
GH_TOKEN=fixture-token GITHUB_OUTPUT="$open_outputs" PATH="$bin_dir:$PATH" \
  "$repo_root/scripts/open_development_bump.sh"
grep -Fxq 'pr_url=https://example.invalid/pull/42' "$open_outputs"
if grep -Eq 'publish_branch_commit|pr create' "$open_record"; then
  echo "repair replaced an already-open cutover" >&2
  exit 1
fi

# Falsifier for the duplicate this script used to open. The drift decision was
# taken while main was still on the released version, the bump it was about
# merged while this job was running, and the job then opened a second one seven
# minutes later. Reproduce that exactly: main already carries the target, and
# no pull request is open for the branch, because the first one merged.
stale_fixture="$tmp_root/stale-workspace"
cp -R "$fixture" "$stale_fixture"
git -C "$stale_fixture" checkout --quiet -- Cargo.toml Cargo.lock
merged="$tmp_root/merged"
git clone --quiet "$origin" "$merged"
git -C "$merged" config user.name "Development Cutover Test"
git -C "$merged" config user.email "development-cutover-test@example.com"
git -C "$merged" config commit.gpgsign false
sed 's/version = "1.2.3"/version = "1.2.4-dev"/' "$merged/Cargo.toml" > "$merged/Cargo.toml.next"
mv "$merged/Cargo.toml.next" "$merged/Cargo.toml"
git -C "$merged" commit --quiet -am "Start 1.2.4-dev development"
git -C "$merged" push --quiet origin HEAD:refs/heads/main

# Repeated repair after cutover is a measured no-op through the same planner.
plan_fixture="$fixture"
fixture="$merged"
run_plan repaired
[[ "$plan_status" == 0 ]] || { cat "$tmp_root/repaired.log" >&2; exit 1; }
grep -Fxq 'required=false' "$plan_outputs"
grep -Fxq 'reason=workspace_does_not_match_latest_stable' "$plan_outputs"
fixture="$plan_fixture"

stale_record="$tmp_root/stale.record"
stale_outputs="$tmp_root/stale.outputs"
CUTOVER_RECORD="$stale_record" \
HARN_RELEASE_ROOT="$stale_fixture" \
HARN_BIN="$bin_dir/harn" \
EXPECTED_DEVELOPMENT_VERSION=1.2.4-dev \
GH_TOKEN=fixture-token \
GITHUB_OUTPUT="$stale_outputs" \
PATH="$bin_dir:$PATH" \
  "$repo_root/scripts/open_development_bump.sh"

grep -Fq 'skipped=true' "$stale_outputs"
grep -Fq 'pr_url=' "$stale_outputs"
if grep -Fq $'gh\tpr create ' "$stale_record"; then
  echo "opener created a second development bump for a version already on main" >&2
  exit 1
fi

# A branch it cannot read is not a branch with nothing on it. Break the remote
# and prove the opener refuses instead of opening on unproved state.
broken_fixture="$tmp_root/broken-workspace"
cp -R "$fixture" "$broken_fixture"
git -C "$broken_fixture" checkout --quiet -- Cargo.toml Cargo.lock
git -C "$broken_fixture" remote set-url origin "$tmp_root/no-such-origin.git"
broken_record="$tmp_root/broken.record"
if CUTOVER_RECORD="$broken_record" \
  HARN_RELEASE_ROOT="$broken_fixture" \
  HARN_BIN="$bin_dir/harn" \
  EXPECTED_DEVELOPMENT_VERSION=1.2.4-dev \
  GH_TOKEN=fixture-token \
  PATH="$bin_dir:$PATH" \
    "$repo_root/scripts/open_development_bump.sh" 2>/dev/null; then
  echo "opener proceeded without being able to read origin/main" >&2
  exit 1
fi
if [[ -f "$broken_record" ]] && grep -Fq $'gh\tpr create ' "$broken_record"; then
  echo "opener created a development bump it could not prove was needed" >&2
  exit 1
fi

# Falsifier: the corpus is red after the PR is opened. The validation fails,
# but the PR creation remains recorded and auto-merge was never armed.
if CUTOVER_RECORD="$record" \
  DEVELOPMENT_BUMP_PR_URL=https://example.invalid/pull/42 \
  FAIL_GRAMMAR_CORPUS=true \
  PATH="$bin_dir:$PATH" \
    "$repo_root/scripts/validate_development_bump.sh"; then
  echo "stale grammar receipt did not fail its own validation" >&2
  exit 1
fi
grep -Fq $'gh\tpr create ' "$record"
if grep -Fq $'gh\tpr merge ' "$record"; then
  echo "red grammar receipt armed the development bump" >&2
  exit 1
fi

# Negative control: a green receipt reaches the explicit auto-merge seam.
CUTOVER_RECORD="$record" \
DEVELOPMENT_BUMP_PR_URL=https://example.invalid/pull/42 \
PATH="$bin_dir:$PATH" \
  "$repo_root/scripts/validate_development_bump.sh"
grep -Fq $'gh\tpr merge https://example.invalid/pull/42 --auto --squash' "$record"

if grep -Fq 'resolved_grammars_pass_the_versioned_fitness_corpus' \
  "$repo_root/scripts/open_development_bump.sh"; then
  echo "development bump opener is still gated on the grammar corpus" >&2
  exit 1
fi

echo "development_bump_cutover_test: ok"
