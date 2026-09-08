#!/usr/bin/env bash
# The agent gate census is the one generated-artifact check with no pre-merge
# CI lane: it runs only on a push to main. This proves the pre-push hook is the
# lane that catches it, that it refuses naming the stale rows, that a clean
# census passes, and that a push touching no recorded reader pays nothing.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
work="$tmp_root/work"
make_record="$tmp_root/make-commands.txt"
mkdir -p "$fake_bin" "$work/.githooks" "$work/spec/agent-gates" \
  "$work/crates/harn-stdlib/src/stdlib/workflow" "$work/crates/harn-parser/src"

# The reader the census records. `changed_reader` moves it; `changed_bystander`
# is a source file with no recorded read, which must cost nothing.
changed_reader="crates/harn-stdlib/src/stdlib/workflow/options.harn"
changed_bystander="crates/harn-parser/src/lexer.rs"

cat > "$work/spec/agent-gates/model-policy-spec.json" <<JSON
[
  {
    "name": "ModelPolicySpec.max_iterations",
    "readers": [
      {
        "file": "$changed_reader",
        "line": 360,
        "reader": "__workflow_agent_loop_options"
      }
    ]
  }
]
JSON

# One JSON report line then the throw, the exact shape check-agent-gates emits.
cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'make %s\n' "$*" >> "$MAKE_COMMAND_RECORD"
case "$*" in
  "-s check-agent-gates")
    if [[ "${CENSUS_RESULT:-pass}" == "pass" ]]; then
      printf '%s\n' '{"entries":516,"failures":[],"pending":0}'
      exit 0
    fi
    printf '%s\n' '{"entries":516,"failures":["stale readers ModelPolicySpec.max_iterations","stale projection docs/src/dev/agent-gates/runner.md"],"pending":2}'
    printf '%s\n' 'error: Thrown: agent gate registry: unregistered reads or stale projections' >&2
    exit 1
    ;;
  lint-actions-source|lint-md)
    exit 0
    ;;
esac
exit 0
SH
chmod +x "$fake_bin/make"

# hook_find_existing_harn_bin returns $HARN_BIN when it is executable, so the
# census never has to resolve or build a real binary here.
cat > "$fake_bin/harn" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$fake_bin/harn"

cat > "$fake_bin/npx" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$fake_bin/npx"

cp "$repo_root/.githooks/lib.sh" "$work/.githooks/lib.sh"
cp "$repo_root/.githooks/pre-push" "$work/.githooks/pre-push"
chmod +x "$work/.githooks/pre-push"

printf '%s\n' 'pipeline options() {}' > "$work/$changed_reader"
printf '%s\n' 'pub fn lex() {}' > "$work/$changed_bystander"

cat > "$fake_bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  "rev-parse --abbrev-ref --symbolic-full-name @{upstream}")
    exit 1
    ;;
  "rev-parse --verify origin/main"|"merge-base HEAD origin/main")
    printf '%s\n' base
    ;;
  "diff --name-only --no-renames --diff-filter=ACMRD base...HEAD")
    printf '%s\n' $CHANGED_PATHS
    ;;
  "diff --name-only -z --no-renames --diff-filter=ACMR base...HEAD -- *.md")
    ;;
  "rev-list base..HEAD")
    ;;
  "rev-parse --abbrev-ref HEAD")
    printf '%s\n' agent-gate-census-test
    ;;
  "rev-parse --show-toplevel")
    pwd
    ;;
  "rev-parse HEAD")
    printf '%s\n' deadbeef
    ;;
  "check-ref-format "*)
    ;;
  *)
    exit 0
    ;;
esac
SH
chmod +x "$fake_bin/git"

update="refs/heads/current cafebabecafebabecafebabecafebabecafebabe refs/heads/current deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"

run_prepush() {
  changed_paths=$1
  census_result=$2
  output=$3
  : > "$make_record"
  set +e
  (
    cd "$work"
    printf '%s' "$update" | \
      CHANGED_PATHS="$changed_paths" \
      CENSUS_RESULT="$census_result" \
      MAKE_COMMAND_RECORD="$make_record" \
      HARN_BIN="$fake_bin/harn" \
      PATH="$fake_bin:$PATH" \
      ./.githooks/pre-push > "$output" 2>&1
  )
  status=$?
  set -e
  return "$status"
}

fail() {
  echo "$1" >&2
  shift
  for extra in "$@"; do
    cat "$extra" >&2
  done
  exit 1
}

# Falsifier: the #8291 shape. A recorded reader moved and the registry was not
# regenerated, so the push is refused and the stale rows are named.
stale_out="$tmp_root/stale.out"
if run_prepush "$changed_reader" fail "$stale_out"; then
  fail "a stale census did not refuse the push" "$stale_out"
fi
grep -Fq "stale readers ModelPolicySpec.max_iterations" "$stale_out" ||
  fail "the refusal did not name the stale registry row" "$stale_out"
grep -Fq "stale projection docs/src/dev/agent-gates/runner.md" "$stale_out" ||
  fail "the refusal did not name the stale projection" "$stale_out"
grep -Fq "make gen-agent-gates" "$stale_out" ||
  fail "the refusal did not name the command that fixes it" "$stale_out"

# Direction control: the same edit with the census regenerated passes.
clean_out="$tmp_root/clean.out"
run_prepush "$changed_reader" pass "$clean_out" ||
  fail "a clean census refused the push" "$clean_out"
grep -Fq "Agent gate census OK." "$clean_out" ||
  fail "a clean census did not report a pass" "$clean_out"

# A push touching no recorded reader must not pay for the census at all.
bystander_out="$tmp_root/bystander.out"
run_prepush "$changed_bystander" fail "$bystander_out" ||
  fail "an unrelated change was charged for the census" "$bystander_out"
grep -Fq "skipping agent gate census" "$bystander_out" ||
  fail "an unrelated change did not report the skip" "$bystander_out"
if grep -Fq "check-agent-gates" "$make_record"; then
  fail "an unrelated change still invoked the census" "$make_record"
fi

# Absence must not read as success. A registry that is present but yields no
# readers is a read the hook failed to account for, not a registry with nothing
# in it, so the census runs rather than being skipped.
mv "$work/spec/agent-gates/model-policy-spec.json" "$tmp_root/readers-away.json"
printf '%s\n' '[]' > "$work/spec/agent-gates/model-policy-spec.json"
unreadable_out="$tmp_root/unreadable.out"
if run_prepush "$changed_bystander" fail "$unreadable_out"; then
  fail "an unreadable registry silently skipped the census" "$unreadable_out"
fi
mv "$tmp_root/readers-away.json" "$work/spec/agent-gates/model-policy-spec.json"

# A tree with no registry at all has no census to keep honest, and must not be
# charged for one.
mv "$work/spec/agent-gates" "$tmp_root/agent-gates-away"
absent_out="$tmp_root/absent.out"
run_prepush "$changed_bystander" fail "$absent_out" ||
  fail "a tree without the registry was charged for the census" "$absent_out"
grep -Fq "skipping agent gate census" "$absent_out" ||
  fail "a tree without the registry did not report the skip" "$absent_out"
mv "$tmp_root/agent-gates-away" "$work/spec/agent-gates"

echo "hook_agent_gate_census_test: ok"
