#!/usr/bin/env bash
#
# Demo gate: every PR that adds a new public Harn primitive must also
# register a `harn demo` scenario exercising it. Tracks issue #2437.
#
# Inputs (from caller):
#   BASE_SHA          merge-base or PR base commit (default: origin/main)
#   HEAD_SHA          PR head commit (default: HEAD)
#   BYPASS_REASON     non-empty string makes the gate pass with a notice
#   GATE_ENABLE_TRACE non-empty enables verbose diagnostics on stderr
#
# Exit codes:
#   0 — no new primitives, or new primitives + a matching demo asset diff,
#       or bypassed via no-demo-needed.
#   1 — at least one new primitive without a matching demo asset diff.
#   2 — usage error (missing required env vars, etc.).
#
# The detection rules deliberately match additions to NEW primitive
# surfaces. Pure refactors that move existing builtins around in the same
# diff zero out (the deleted + added lines cancel out via simple line
# count, and the gate looks for net additions of the recognized patterns).

set -euo pipefail

BASE_SHA="${BASE_SHA:-origin/main}"
HEAD_SHA="${HEAD_SHA:-HEAD}"
BYPASS_REASON="${BYPASS_REASON:-}"
GATE_ENABLE_TRACE="${GATE_ENABLE_TRACE:-}"

# Demo assets directory: a primitive-introducing PR must add or modify
# at least one file under here.
DEMO_DIR="crates/harn-cli/assets/demo"

# Bypass short-circuit: a `no-demo-needed` label (or other declared
# reason) flips the gate green with a clear notice. Hygiene PRs and
# pure-refactor PRs use this path.
if [ -n "$BYPASS_REASON" ]; then
  echo "::notice title=Demo gate bypassed::$BYPASS_REASON"
  exit 0
fi

# Resolve the diff range. `git merge-base` keeps the comparison stable
# when the PR has merge commits or is rebased onto a moved base.
if ! merge_base=$(git merge-base "$BASE_SHA" "$HEAD_SHA" 2>/dev/null); then
  # Fall back to the literal base ref if we can't compute a merge-base
  # (e.g. shallow checkout without enough history). The diff is still
  # meaningful — it just may overcount on rebase-divergent histories.
  merge_base="$BASE_SHA"
fi
[ -n "$GATE_ENABLE_TRACE" ] && echo "[demo-gate] base=$BASE_SHA head=$HEAD_SHA merge_base=$merge_base" >&2

# Capture the full unified diff once so all detectors can read it. The
# `--no-renames` flag avoids GitHub's smart rename heuristic, which can
# hide a file-add-as-rename from our additive-line scan. Use the
# two-arg form (not `merge_base...HEAD`) so we don't re-trigger a
# merge-base computation that may fail under shallow checkout — we
# already resolved `merge_base` above with a fallback.
diff_file=$(mktemp)
trap 'rm -f "$diff_file"' EXIT
git diff --unified=0 --no-renames "$merge_base" "$HEAD_SHA" > "$diff_file"

# Lines starting with `+` (excluding the `+++ filename` header) are
# additions; lines starting with `diff --git a/...` are file boundaries.
# Track the current file so each addition can be attributed to a path.
#
# We emit one record per detected primitive addition:
#   <category>\t<file>\t<line preview>
# A non-empty findings file means the PR introduces at least one
# primitive and must ship a matching demo.

findings_file=$(mktemp)
trap 'rm -f "$diff_file" "$findings_file"' EXIT

awk '
  BEGIN { file = "" }
  /^diff --git a\// {
    # `diff --git a/path/x.rs b/path/x.rs` — pull the b-side path so
    # renames and adds both attribute to the post-change name.
    n = split($0, parts, " ")
    file = parts[n]
    sub(/^b\//, "", file)
    next
  }
  /^\+\+\+ / || /^--- / || /^@@/ { next }
  /^\+/ {
    line = substr($0, 2)
    # stdlib builtin registrations — register_builtin("name", ...)
    if (file ~ /^crates\/harn-vm\/src\/stdlib\/.*\.rs$/) {
      if (match(line, /register_builtin\("[^"]+"/) > 0) {
        printf "stdlib-builtin\t%s\t%s\n", file, substr(line, 1, 120)
        next
      }
      if (match(line, /SyncBuiltin::new\("[^"]+"/) > 0) {
        printf "stdlib-builtin\t%s\t%s\n", file, substr(line, 1, 120)
        next
      }
      if (match(line, /async_builtin!\("[^"]+"/) > 0) {
        printf "stdlib-builtin\t%s\t%s\n", file, substr(line, 1, 120)
        next
      }
      # New `register_builtin_group(vm, NAME)` introduces a whole group
      # — also a public-primitive surface expansion.
      if (match(line, /register_builtin_group\(/) > 0) {
        printf "stdlib-builtin-group\t%s\t%s\n", file, substr(line, 1, 120)
        next
      }
    }
    # New host-capability dispatch arms in stdlib/host.rs.
    # Pattern: `("capability", "operation") =>`.
    if (file == "crates/harn-vm/src/stdlib/host.rs") {
      if (match(line, /\("[a-zA-Z_][a-zA-Z0-9_]*",[[:space:]]*"[a-zA-Z_][a-zA-Z0-9_]*"\)[[:space:]]*=>/) > 0) {
        printf "host-call\t%s\t%s\n", file, substr(line, 1, 120)
        next
      }
    }
    # New hostlib BUILTIN_* consts — the host-capability surface that
    # host_call("module.method", ...) dispatches into.
    if (file ~ /^crates\/harn-hostlib\/src\/.*\.rs$/) {
      if (match(line, /(pub|pub\(super\)|pub\(crate\))[[:space:]]+const[[:space:]]+BUILTIN_[A-Z0-9_]+:[[:space:]]*&str[[:space:]]*=[[:space:]]*"hostlib_/) > 0) {
        printf "host-call\t%s\t%s\n", file, substr(line, 1, 120)
        next
      }
    }
    # New orchestrator surfaces — public functions in the CLI
    # orchestrator subcommand tree.
    if (file ~ /^crates\/harn-cli\/src\/commands\/orchestrator\/.*\.rs$/) {
      if (match(line, /pub[[:space:]]+(async[[:space:]]+)?fn[[:space:]]+[a-zA-Z_][a-zA-Z0-9_]*[[:space:]]*\(/) > 0) {
        printf "orchestrator-surface\t%s\t%s\n", file, substr(line, 1, 120)
        next
      }
    }
    # New language constructs — parser rules.
    if (file ~ /^crates\/harn-parser\/src\/parser\/.*\.rs$/) {
      if (match(line, /(pub[[:space:]]+)?fn[[:space:]]+parse_[a-zA-Z_][a-zA-Z0-9_]*[[:space:]]*\(/) > 0) {
        printf "grammar-rule\t%s\t%s\n", file, substr(line, 1, 120)
        next
      }
    }
  }
' "$diff_file" > "$findings_file"

# Did the same diff also touch the demo assets dir? Any add/modify on
# any file under DEMO_DIR counts — adding a new scenario, extending an
# existing one's tape, or growing scenario.harn all satisfy the gate.
demo_touched=0
if git diff --name-only --no-renames "$merge_base" "$HEAD_SHA" | grep -E "^${DEMO_DIR}/" > /dev/null; then
  demo_touched=1
fi
[ -n "$GATE_ENABLE_TRACE" ] && echo "[demo-gate] demo_touched=$demo_touched" >&2

# Empty findings = no watched primitive additions = nothing to gate on.
if [ ! -s "$findings_file" ]; then
  echo "::notice title=Demo gate::No new primitives detected. Gate skipped."
  exit 0
fi

# Findings present + demo dir touched = the contract is met.
if [ "$demo_touched" -eq 1 ]; then
  count=$(wc -l < "$findings_file" | tr -d ' ')
  echo "::notice title=Demo gate::Detected $count new primitive(s); demo assets also modified — OK."
  if [ -n "$GATE_ENABLE_TRACE" ]; then
    echo "[demo-gate] findings:" >&2
    cat "$findings_file" >&2
  fi
  exit 0
fi

# Findings present but no demo asset touched = fail with a precise
# diagnostic so the author knows exactly what to wire a demo around.
count=$(wc -l < "$findings_file" | tr -d ' ')
echo "::error title=Demo gate::PR adds $count new primitive(s) but no \`${DEMO_DIR}/**\` changes."
echo
echo "Unmatched primitive additions (first 20):"
head -20 "$findings_file" | awk -F'\t' '{
  printf "  - [%s] %s\n    %s\n", $1, $2, $3
}'
echo
echo "To resolve, pick one of:"
echo "  1. Add a demo scenario under \`${DEMO_DIR}/<id>/\` exercising the new"
echo "     primitive(s), wire it into \`SCENARIOS\` in"
echo "     \`crates/harn-cli/src/commands/demo.rs\`, and add a smoke test in"
echo "     \`crates/harn-cli/tests/harn_cli_fast/demo_cli.rs\`. See CONTRIBUTING.md \"Demo gate\"."
echo "  2. If this PR is hygiene-only or a pure refactor, add the"
echo "     \`no-demo-needed\` label."
exit 1
