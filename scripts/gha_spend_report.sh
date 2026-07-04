#!/usr/bin/env bash
set -uo pipefail

# Print estimated GitHub Actions spend per repo for the burin-labs org,
# sorted most-expensive first, using `gh api`. Companion to
# scripts/hook_timings_report.sh: together they surface both build-time
# regressions (per-hook durations) and their downstream CI cost.
#
# Primary source: GET /orgs/{org}/settings/billing/usage — the current org
# usage-report endpoint. It reports actual metered minutes and net USD per
# repo+SKU+month. The older /settings/billing/actions endpoint has been
# retired by GitHub (HTTP 410) and is not used here.
#
# Fallback (used when the billing endpoint is inaccessible, e.g. the `gh`
# token lacks org-billing scope): sums each run's billable minutes via
# `/repos/{owner}/{repo}/actions/runs/{id}/timing`, grouped by workflow name,
# using published per-minute list prices as an estimate. Cost in this mode
# is an estimate, not a metered figure.
#
# Usage:
#   scripts/gha_spend_report.sh
#   scripts/gha_spend_report.sh --org burin-labs --repo harn --repo burin-code
#   scripts/gha_spend_report.sh --json

org="burin-labs"
repos=()
json_output=0
runs_per_repo=50

while [ $# -gt 0 ]; do
  case "$1" in
    --org)
      org=$2
      shift 2
      ;;
    --repo)
      repos+=("$2")
      shift 2
      ;;
    --json)
      json_output=1
      shift
      ;;
    *)
      echo "gha_spend_report.sh: unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

if [ ${#repos[@]} -eq 0 ]; then
  repos=(harn burin-code)
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "gha_spend_report.sh: gh CLI not found" >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "gha_spend_report.sh: python3 not found (used to aggregate the gh api JSON)" >&2
  exit 1
fi

year=$(date -u +%Y)
billing_json=$(gh api "/orgs/${org}/settings/billing/usage?year=${year}" 2>/dev/null || true)

report_billing_usage() {
  # Aggregate usageItems (product=actions, unitType=Minutes) by repo,
  # summing quantity (minutes) and netAmount (USD).
  printf '%s' "$billing_json" | python3 -c '
import json, sys

data = json.load(sys.stdin)
totals = {}
for item in data.get("usageItems", []):
    if item.get("product") != "actions" or item.get("unitType") != "Minutes":
        continue
    repo = item.get("repositoryName", "unknown")
    entry = totals.setdefault(repo, {"minutes": 0.0, "usd": 0.0})
    entry["minutes"] += item.get("quantity", 0.0)
    entry["usd"] += item.get("netAmount", 0.0)

rows = sorted(totals.items(), key=lambda kv: kv[1]["usd"], reverse=True)
header_workflow = "workflow"
header_minutes = "minutes"
header_usd = "estimated_usd"
print("%-28s %10s %14s  source" % (header_workflow, header_minutes, header_usd))
print("%-28s %10s %14s  %s" % ("-" * 28, "-" * 10, "-" * 14, "-" * 13))
for repo, totals_row in rows:
    label = "repo:" + repo
    print("%-28s %10.2f %14.2f  billing-usage" % (label, totals_row["minutes"], totals_row["usd"]))
'
}

report_run_timing_fallback() {
  echo "(billing usage endpoint unavailable or unauthorized; falling back to run /timing minutes)" >&2
  for repo in "${repos[@]}"; do
    runs_json=$(gh api "/repos/${org}/${repo}/actions/runs?per_page=${runs_per_repo}" 2>/dev/null || true)
    [ -z "$runs_json" ] && continue
    run_ids=$(printf '%s' "$runs_json" | python3 -c 'import json,sys; [print(r["id"], r["name"]) for r in json.load(sys.stdin).get("workflow_runs", [])]')
    [ -z "$run_ids" ] && continue
    while IFS=' ' read -r run_id workflow_name; do
      [ -z "$run_id" ] && continue
      timing_json=$(gh api "/repos/${org}/${repo}/actions/runs/${run_id}/timing" 2>/dev/null || true)
      [ -z "$timing_json" ] && continue
      printf '%s\t%s\t%s\n' "$repo" "$workflow_name" "$timing_json"
    done <<< "$run_ids"
  done | python3 -c '
import json, sys

PRICE_PER_MINUTE = {"UBUNTU": 0.008, "WINDOWS": 0.016, "MACOS": 0.08}
totals = {}
for line in sys.stdin:
    repo, workflow, timing_raw = line.rstrip("\n").split("\t", 2)
    timing = json.loads(timing_raw)
    key = repo + ":" + workflow
    entry = totals.setdefault(key, {"minutes": 0.0, "usd": 0.0})
    for runner_os, billable in timing.get("billable", {}).items():
        minutes = billable.get("total_ms", 0) / 60000
        entry["minutes"] += minutes
        entry["usd"] += minutes * PRICE_PER_MINUTE.get(runner_os, 0.0)

rows = sorted(totals.items(), key=lambda kv: kv[1]["usd"], reverse=True)
print("%-28s %10s %14s  source" % ("workflow", "minutes", "estimated_usd"))
print("%-28s %10s %14s  %s" % ("-" * 28, "-" * 10, "-" * 14, "-" * 13))
for key, totals_row in rows:
    print("%-28s %10.2f %14.2f  run-timing" % (key, totals_row["minutes"], totals_row["usd"]))
'
}

if [ "$json_output" -eq 1 ]; then
  if [ -n "$billing_json" ] && printf '%s' "$billing_json" | python3 -c 'import json,sys; json.load(sys.stdin)["usageItems"]' >/dev/null 2>&1; then
    printf '%s' "$billing_json"
  else
    echo '{"error":"billing usage endpoint unavailable; rerun without --json for the run-timing fallback table"}'
  fi
  exit 0
fi

if [ -n "$billing_json" ] && printf '%s' "$billing_json" | python3 -c 'import json,sys; json.load(sys.stdin)["usageItems"]' >/dev/null 2>&1; then
  echo "GitHub Actions spend report for org \"${org}\" (source: billing usage report)"
  report_billing_usage
else
  echo "GitHub Actions spend report for org \"${org}\""
  report_run_timing_fallback
fi
