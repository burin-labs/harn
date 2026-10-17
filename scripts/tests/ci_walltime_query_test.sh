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
    printf '{"workflow_runs":[{"id":%s,"head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","status":"completed","conclusion":"success","created_at":"2026-10-%sT00:00:00Z","updated_at":"2026-10-%sT00:12:00Z","html_url":"https://fixture.test/run/%s"}]}\n' "$id" "$day" "$day" "$id"
    ;;
  /repos/fixture/repo/actions/runs/2/jobs\?*)
    if [[ -f "$(dirname "$0")/../evidence-mode" ]]; then
      if [[ "$(cat "$(dirname "$0")/../evidence-mode")" == missing-census ]]; then
        echo '{}'
        exit 0
      fi
      printf '%s\n' '{"jobs":[{"id":41,"run_id":2,"name":"Full acceptance","status":"in_progress","conclusion":null,"started_at":"2026-10-02T00:00:10Z","completed_at":null,"check_run_url":"https://api.github.com/repos/fixture/repo/check-runs/47","steps":[]}]}'
    else
      printf '%s\n' '{"jobs":[{"name":"Full acceptance","conclusion":"success","started_at":"2026-10-02T00:00:10Z","completed_at":"2026-10-02T00:12:00Z","steps":[]}]}'
    fi
    ;;
  /repos/fixture/repo/check-runs/47)
    printf '%s\n' "$2" >> "$(dirname "$0")/../check-requests"
    case "$(cat "$(dirname "$0")/../evidence-mode")" in
      missing) echo 'gh: HTTP 404' >&2; exit 1 ;;
      forbidden) echo 'gh: HTTP 403' >&2; exit 1 ;;
      malformed) echo '{invalid-json'; exit 0 ;;
      terminal)
        printf '%s\n' '{"id":47,"name":"Full acceptance","url":"https://api.github.com/repos/fixture/repo/check-runs/47","details_url":"https://github.com/fixture/repo/actions/runs/2/job/41","head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","app":{"slug":"github-actions"},"status":"completed","conclusion":"success","started_at":"2026-10-02T00:00:10Z","completed_at":"2026-10-02T00:12:00Z"}'
        ;;
      *) exit 2 ;;
    esac
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

# An optional secondary lookup cannot make absence look like a terminal job.
# Exercise the real process adapter, including failed exit and malformed JSON.
for mode in missing forbidden malformed terminal; do
  printf '%s\n' "$mode" > "$tmp/evidence-mode"
  : > "$tmp/check-requests"
  PATH="$tmp/bin:$PATH" "$HARN_BIN" run --standalone --no-sandbox \
    "$repo_root/scripts/ci_walltime_report.harn" -- --policy "$tmp/policy.json" --json \
    > "$tmp/$mode.json" 2> "$tmp/$mode.stderr"
  [[ "$(wc -l < "$tmp/check-requests" | tr -d ' ')" == 1 ]]
  if [[ "$mode" == terminal ]]; then
    jq -e '(.qualifying_runs|length) == 1 and .qualifying_runs[0].id == 2 and .wall.p90_ms == 720000 and .evaluation.p90_breach == true' "$tmp/$mode.json" >/dev/null
    grep -F 'Verified terminal check evidence for run 2' "$tmp/$mode.stderr" >/dev/null
  else
    jq -e '.candidate_runs == 1 and (.qualifying_runs|length) == 0 and .evaluation.ok == false and .evaluation.status == "insufficient_samples"' "$tmp/$mode.json" >/dev/null
    grep -F 'pending=1 [Full acceptance] failing=0 []' "$tmp/$mode.stderr" >/dev/null
  fi
done
echo 'ci_walltime_query_test: unavailable optional checks stay pending; exact terminal check recovers'

printf '%s\n' missing-census > "$tmp/evidence-mode"
if PATH="$tmp/bin:$PATH" "$HARN_BIN" run --standalone --no-sandbox \
  "$repo_root/scripts/ci_walltime_report.harn" -- --policy "$tmp/policy.json" --json \
  > "$tmp/missing-census.json" 2> "$tmp/missing-census.stderr"; then
  echo 'error: absent primary jobs census was accepted' >&2
  exit 1
fi
[[ ! -s "$tmp/missing-census.json" ]]
echo 'ci_walltime_query_test: absent primary jobs census fails loudly'
