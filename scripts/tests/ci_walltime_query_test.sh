#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
[[ -x "${HARN_BIN:-}" ]] || { echo 'HARN_BIN must name the existing CLI' >&2; exit 1; }
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
cat > "$tmp/policy.json" <<'JSON'
{"schema_version":1,"repository":"fixture/repo","workflow":"ci.yml","event":"merge_group","topology_epoch":"2026-10-01T00:00:00Z","full_run_jobs":[{"name":"Full acceptance","match":"exact"}],"slo":{"warning_ms":540000,"p90_ms":600000,"hard_max_ms":900000,"min_samples":1,"window_samples":1}}
JSON
cat > "$tmp/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == api ]] || exit 2
case "$2" in
  /repos/fixture/repo/actions/workflows/ci.yml/runs\?*)
    # Observed API failure: an unbounded status filter returned old successful
    # runs despite newer full runs. Apply the policy epoch at the API boundary.
    if [[ "$2" == *'created=%3E%3D2026-10-01T00:00:00Z'* ]]; then
      id=2; day=02
    else
      id=1; day=01
    fi
    printf '{"workflow_runs":[{"id":%s,"created_at":"2026-10-%sT00:00:00Z","updated_at":"2026-10-%sT00:12:00Z","html_url":"https://fixture.test/run/%s"}]}\n' "$id" "$day" "$day" "$id"
    ;;
  /repos/fixture/repo/actions/runs/2/jobs\?*)
    printf '%s\n' '{"jobs":[{"name":"Full acceptance","conclusion":"success","started_at":"2026-10-02T00:00:10Z","completed_at":"2026-10-02T00:12:00Z","steps":[]}]}'
    ;;
  /repos/fixture/repo/actions/runs/1/jobs\?*) printf '%s\n' '{"jobs":[]}' ;;
  *) exit 2 ;;
esac
SH
chmod +x "$tmp/bin/gh"
PATH="$tmp/bin:$PATH" "$HARN_BIN" run --standalone --no-sandbox \
  "$repo_root/scripts/ci_walltime_report.harn" -- --policy "$tmp/policy.json" --json > "$tmp/report.json"
jq -e '.candidate_runs == 1 and (.qualifying_runs|length) == 1 and .qualifying_runs[0].id == 2 and .wall.p90_ms == 720000 and .evaluation.p90_breach == true' "$tmp/report.json" >/dev/null
echo 'ci_walltime_query_test: current full run measured; latency breach retained'
