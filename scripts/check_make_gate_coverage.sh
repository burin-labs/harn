#!/usr/bin/env bash
#
# Gate-coverage ratchet: every target in `make all` must actually run in CI,
# or be registered as intentionally local with a reason.
#
# The failure this exists to catch is silent. A gate target gets added to
# `make all`, nobody wires it into a workflow, and it then runs only on a
# developer's machine — green on every PR because it never executes. This has
# already happened at least three times in this repo:
#   * `scripts/audit_gates.sh:91` — "Was in `make all` but in no workflow, so
#     nothing watched it: the suite sat red on main for a source-scope
#     regression until someone ran it by hand."
#   * `.github/workflows/ci.yml:1181` — a second round, for the workflow-file
#     contracts.
#   * `test-pr-gate-scripts` — ~56 shell gate tests, unreferenced by any
#     workflow until this check landed.
#
# A target counts as reached when it is named on a `make ...` line inside
# `.github/`, or listed in the `GATES` array that `scripts/audit_gates.sh`
# fans out. Anything else must appear in the registry with a reason. There is
# deliberately no dependency-graph resolution here: a target reached only as
# some other target's prerequisite is registered explicitly, so the reason is
# written down rather than inferred by this script.
#
# Exit codes:
#   0 — every `make all` target is reached or registered.
#   1 — an unregistered target would run only locally, or a registry entry is stale.
#   2 — usage/parse error.

set -euo pipefail

# Default to this script's repository, but let a caller point the check at a
# fixture tree. Without this override the checker would always re-read the real
# repo and its own positive control could never fail.
repo_root="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$repo_root"

REGISTRY="${REGISTRY:-.github/make-gate-coverage.json}"
MAKEFILE="${MAKEFILE:-Makefile}"
AUDIT_GATES="${AUDIT_GATES:-scripts/audit_gates.sh}"
WORKFLOW_DIR="${WORKFLOW_DIR:-.github}"

for required in "$REGISTRY" "$MAKEFILE" "$AUDIT_GATES"; do
  if [ ! -f "$required" ]; then
    echo "check_make_gate_coverage: missing required input: $required" >&2
    exit 2
  fi
done

# --- the population: targets `make all` fans out to -------------------------
# `all:` runs a single recursive $(MAKE) whose arguments are the gate list.
all_targets=$(
  awk '
    /^all:/ { in_all = 1 }
    in_all && /\$\(MAKE\) HARN_BIN=/ {
      sub(/^.*\$\(MAKE\) HARN_BIN="\$\$harn_bin"[[:space:]]*/, "")
      print
      exit
    }
  ' "$MAKEFILE" | tr ' \t' '\n' | grep -E '^[a-zA-Z0-9_-]+$' | sort -u
)

if [ -z "$all_targets" ]; then
  echo "check_make_gate_coverage: could not parse the target list out of \`all:\` in $MAKEFILE" >&2
  echo "  (the recipe shape changed; update this parser rather than deleting the check)" >&2
  exit 2
fi

# --- reached set A: named on a `make ...` line anywhere under .github/ -------
# Captures every target on the line, so `make lint lint-md` credits both.
workflow_reached=$(
  grep -rhoE '\bmake[[:space:]]+([a-zA-Z0-9_-]+[[:space:]]+)*[a-zA-Z0-9_-]+' "$WORKFLOW_DIR" 2>/dev/null \
    | sed -E 's/^make[[:space:]]+//' | tr ' \t' '\n' | grep -E '^[a-zA-Z0-9_-]+$' | sort -u || true
)

# --- reached set B: the GATES array that audit_gates.sh fans out ------------
gates_reached=$(
  awk '/^GATES=\(/ { in_gates = 1; next } in_gates && /^\)/ { exit } in_gates { print }' "$AUDIT_GATES" \
    | sed -E 's/#.*$//' | tr -d ' \t' | grep -E '^[a-zA-Z0-9_-]+$' | sort -u || true
)
# audit_gates.sh runs `conformance` explicitly, outside the GATES fanout.
gates_reached=$(printf '%s\nconformance\n' "$gates_reached" | grep -E '^[a-zA-Z0-9_-]+$' | sort -u)

reached=$(printf '%s\n%s\n' "$workflow_reached" "$gates_reached" | sort -u)

# --- the registry of intentionally-local targets ----------------------------
# Structure first, contents second: an EMPTY registry is valid and must not be
# reported as an unreadable one.
if ! jq -e '(.intentionally_local | type) == "array"' "$REGISTRY" >/dev/null 2>&1; then
  echo "check_make_gate_coverage: $REGISTRY is not readable, or has no .intentionally_local array" >&2
  exit 2
fi
registered=$(jq -r '.intentionally_local[].target' "$REGISTRY" | sort -u)
if ! jq -e '.intentionally_local | all(has("target") and has("reason") and (.reason | length > 0))' "$REGISTRY" >/dev/null; then
  echo "check_make_gate_coverage: every $REGISTRY entry needs a non-empty \`target\` and \`reason\`" >&2
  exit 1
fi

# --- verdict ----------------------------------------------------------------
orphans=$(comm -23 <(printf '%s\n' "$all_targets") <(printf '%s\n' "$reached" "$registered" | sort -u))
# A registry row for a target that IS reached, or that `make all` no longer
# runs, is stale — drop it so the registry cannot accumulate dead rows.
stale=$(comm -12 <(printf '%s\n' "$registered") <(printf '%s\n' "$reached"))
gone=$(comm -23 <(printf '%s\n' "$registered") <(printf '%s\n' "$all_targets"))

status=0
if [ -n "$orphans" ]; then
  status=1
  {
    echo "::error title=Gate coverage::\`make all\` target(s) run in NO workflow, so they gate nothing."
    echo ""
    printf '%s\n' "$orphans" | sed 's/^/  - /'
    echo ""
    echo "Wire each one into a workflow, add it to GATES in $AUDIT_GATES,"
    echo "or register it in $REGISTRY with a reason."
  } >&2
fi
if [ -n "$stale" ]; then
  status=1
  { echo "::error title=Gate coverage::stale $REGISTRY row(s) — these targets ARE reached; remove them:"
    printf '%s\n' "$stale" | sed 's/^/  - /'; } >&2
fi
if [ -n "$gone" ]; then
  status=1
  { echo "::error title=Gate coverage::stale $REGISTRY row(s) — \`make all\` no longer runs these:"
    printf '%s\n' "$gone" | sed 's/^/  - /'; } >&2
fi

if [ "$status" -eq 0 ]; then
  echo "check_make_gate_coverage: ok — $(printf '%s\n' "$all_targets" | wc -l | tr -d ' ') \`make all\` targets, $(printf '%s\n' "$registered" | grep -c . || true) registered as local."
fi
exit "$status"
