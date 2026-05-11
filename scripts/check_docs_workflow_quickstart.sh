#!/usr/bin/env bash
#
# Verify the workflow-authoring quickstart fixtures still produce the
# output the docs page promises. The quickstart at
# `docs/src/workflow-authoring-quickstart.md` shows readers exact bundle
# digests, executed-node sequences, and connector-status shapes; those
# claims must stay true on every commit so the copy-paste path does not
# rot.
#
# We hit three surfaces:
#   * `harn workflow validate --json`       — pinned graph_digest
#   * `harn workflow run --json`            — executed_nodes + status
#   * `harn connect status / setup-plan --json` — shape from the
#                                            docs/fixtures/connect-demo
#                                            manifest
#
# Exits non-zero on the first mismatch.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

HARN_BIN="${HARN_BIN:-}"
if [[ -z "$HARN_BIN" ]]; then
  target_dir=""
  if command -v cargo >/dev/null 2>&1; then
    target_dir="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
      | python3 -c 'import json,sys; print(json.load(sys.stdin).get("target_directory", ""))' 2>/dev/null)"
  fi
  if [[ -z "$target_dir" ]]; then
    target_dir="${CARGO_TARGET_DIR:-target}"
  fi
  if [[ -x "$target_dir/debug/harn" ]]; then
    HARN_BIN="$target_dir/debug/harn"
  else
    echo "building harn-cli (set HARN_BIN to skip)..." >&2
    cargo build -q -p harn-cli
    HARN_BIN="$target_dir/debug/harn"
  fi
fi

# JSON path assertion. Uses python3 (already a `make all` dep via other
# scripts) so we do not pull in jq.
expect_json_path() {
  local label="$1"
  local json="$2"
  local path="$3"
  local expected="$4"
  local actual
  actual="$(printf '%s' "$json" | python3 -c "
import json, sys
doc = json.load(sys.stdin)
parts = sys.argv[1].split('.') if sys.argv[1] else []
node = doc
for part in parts:
    if part.endswith(']'):
        name, idx = part[:-1].split('[')
        if name:
            node = node[name]
        node = node[int(idx)]
    else:
        node = node[part]
print(json.dumps(node))
" "$path")"
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL: $label" >&2
    echo "  path:     $path" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    exit 1
  fi
}

echo "=== Checking workflow quickstart fixtures ==="

# --- 1. Minimal bundle ---
MIN_BUNDLE="docs/fixtures/workflow-bundles/quickstart-minimal.bundle.json"

VALIDATE_JSON="$("$HARN_BIN" workflow validate --bundle "$MIN_BUNDLE" --json)"
expect_json_path "minimal validate.valid"        "$VALIDATE_JSON" "valid"        "true"
expect_json_path "minimal validate.bundle_id"    "$VALIDATE_JSON" "bundle_id"    '"quickstart-minimal"'
expect_json_path "minimal validate.workflow_id"  "$VALIDATE_JSON" "workflow_id"  '"quickstart_minimal_workflow"'
expect_json_path "minimal validate.errors empty" "$VALIDATE_JSON" "errors"       "[]"

MIN_DIGEST="$(printf '%s' "$VALIDATE_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["graph_digest"])')"

RUN_JSON="$("$HARN_BIN" workflow run --bundle "$MIN_BUNDLE" --json)"
expect_json_path "minimal run.status"             "$RUN_JSON" "status"           '"completed"'
expect_json_path "minimal run.run_id (pinned)"    "$RUN_JSON" "run_id"           '"bundle_run_quickstart_minimal_fixture"'
expect_json_path "minimal run.trigger_id"         "$RUN_JSON" "trigger_id"       '"manual-start"'
expect_json_path "minimal run.executed_nodes[0]"  "$RUN_JSON" "executed_nodes[0].node_id" '"summarize"'
expect_json_path "minimal run.executed_nodes[1]"  "$RUN_JSON" "executed_nodes[1].node_id" '"notify"'
expect_json_path "minimal run.graph_digest"       "$RUN_JSON" "graph_digest"     "\"$MIN_DIGEST\""

# Pinned digest in the docs page — bump both together if the canonical
# graph encoding intentionally changes.
EXPECTED_MIN_DIGEST='sha256:2f127f6b03ae4837f0e78b572774447cb644c0085bd561cd78eff663d1ce27f8'
if [[ "$MIN_DIGEST" != "$EXPECTED_MIN_DIGEST" ]]; then
  echo "FAIL: minimal bundle graph_digest drifted from the value pinned in" >&2
  echo "      docs/src/workflow-authoring-quickstart.md" >&2
  echo "  expected: $EXPECTED_MIN_DIGEST" >&2
  echo "  actual:   $MIN_DIGEST" >&2
  echo "  fix:      update both the docs page and this script if the change is intentional" >&2
  exit 1
fi

# --- 2. Agentic bundle ---
AGENT_BUNDLE="docs/fixtures/workflow-bundles/quickstart-agentic.bundle.json"

VALIDATE_JSON="$("$HARN_BIN" workflow validate --bundle "$AGENT_BUNDLE" --json)"
expect_json_path "agentic validate.valid"        "$VALIDATE_JSON" "valid"       "true"
expect_json_path "agentic validate.bundle_id"    "$VALIDATE_JSON" "bundle_id"   '"quickstart-agentic"'
expect_json_path "agentic validate.errors empty" "$VALIDATE_JSON" "errors"      "[]"

RUN_JSON="$("$HARN_BIN" workflow run --bundle "$AGENT_BUNDLE" --json)"
expect_json_path "agentic run.status"            "$RUN_JSON" "status"             '"completed"'
expect_json_path "agentic run.trigger_id"        "$RUN_JSON" "trigger_id"         '"github-pr-opened"'
expect_json_path "agentic run.executed_nodes[1]" "$RUN_JSON" "executed_nodes[1].node_id" '"review"'
expect_json_path "agentic run.review.kind"       "$RUN_JSON" "executed_nodes[1].kind"    '"agent"'
expect_json_path "agentic run.review.capsule"    "$RUN_JSON" "executed_nodes[1].prompt_capsule" '"draft-review"'

# Agentic bundle must continue to declare exactly one connector
# requirement; the docs walkthrough relies on this.
expect_json_path "agentic run.connectors[0]"     "$RUN_JSON" "connectors[0].id" '"github"'

# --- 3. Connect demo manifest ---
DEMO_DIR="docs/fixtures/connect-demo"

STATUS_JSON="$(cd "$DEMO_DIR" && "$HARN_BIN" connect status --json)"
expect_json_path "demo status.connectors[0].id"          "$STATUS_JSON" "connectors[0].id"          '"demo"'
expect_json_path "demo status.connectors[0].installed"   "$STATUS_JSON" "connectors[0].installed"   "true"
expect_json_path "demo status.connectors[0].usable"      "$STATUS_JSON" "connectors[0].usable"      "false"
expect_json_path "demo status.connectors[0].status"      "$STATUS_JSON" "connectors[0].status"      '"missing_auth"'
expect_json_path "demo status.connectors[0].auth_type"   "$STATUS_JSON" "connectors[0].auth_type"   '"api-key"'

PLAN_JSON="$(cd "$DEMO_DIR" && "$HARN_BIN" connect setup-plan --connector demo --json)"
expect_json_path "demo setup-plan.connector"   "$PLAN_JSON" "connector"   '"demo"'
expect_json_path "demo setup-plan.auth_type"   "$PLAN_JSON" "auth_type"   '"api-key"'
expect_json_path "demo setup-plan.steps[0].id" "$PLAN_JSON" "steps[0].id" '"authorize"'

echo "    Workflow quickstart fixtures OK."
