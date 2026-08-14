#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/discover_candidate_archive_run.sh \
  --repo OWNER/REPO --source-commit SHA [--workflow build-release-binaries.yml]

Finds the newest successful workflow_dispatch run that produced unexpired
candidate archive artifacts for the requested source commit. A later successful
run supersedes an earlier retry for the same immutable source. Prints only the
run id on stdout.
EOF
}

repo=""
source_commit=""
workflow="build-release-binaries.yml"

while (($#)); do
  case "$1" in
    --repo) repo="${2:-}"; shift 2 ;;
    --source-commit) source_commit="${2:-}"; shift 2 ;;
    --workflow) workflow="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$repo" || -z "$source_commit" ]]; then
  echo "error: --repo and --source-commit are required" >&2
  usage >&2
  exit 2
fi
if [[ ! "$repo" =~ ^[^/]+/[^/]+$ ]]; then
  echo "error: expected repo OWNER/REPO, got '$repo'" >&2
  exit 2
fi
if [[ ! "$source_commit" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: --source-commit must be a 40-character lowercase hex SHA" >&2
  exit 2
fi

manifest_artifact="candidate-archive-manifest-$source_commit"
matching_run_ids=()

runs_json="$(gh api "repos/$repo/actions/runs?per_page=50&status=completed&event=workflow_dispatch")"
while IFS= read -r run_json; do
  [[ -n "$run_json" ]] || continue
  run_id="$(jq -r '.id // empty' <<<"$run_json")"
  conclusion="$(jq -r '.conclusion // empty' <<<"$run_json")"
  workflow_path="$(jq -r '.path // empty' <<<"$run_json")"
  display_title="$(jq -r '.display_title // empty' <<<"$run_json")"
  run_name="$(jq -r '.name // empty' <<<"$run_json")"

  [[ -n "$run_id" && "$conclusion" == "success" ]] || continue
  if [[ "$(basename "$workflow_path")" != "$workflow" ]]; then
    continue
  fi

  matched=false
  artifacts_json="$(gh api "repos/$repo/actions/runs/$run_id/artifacts")"
  if jq -e --arg name "$manifest_artifact" \
    'any(.artifacts[]?; .name == $name and ((.expired // false) | not))' \
    <<<"$artifacts_json" >/dev/null; then
    matched=true
  elif jq -e --arg name "$manifest_artifact" \
    'any(.artifacts[]?; .name == $name)' <<<"$artifacts_json" >/dev/null; then
    # An explicitly expired manifest is not reusable. Do not let the legacy
    # title fallback resurrect it.
    matched=false
  elif [[ "$display_title" == *"$source_commit"* || "$run_name" == *"$source_commit"* ]]; then
    matched=true
  fi

  if [[ "$matched" == "true" ]]; then
    matching_run_ids+=("$run_id")
  fi
done < <(jq -c '.workflow_runs[]?' <<<"$runs_json")

if ((${#matching_run_ids[@]} == 0)); then
  echo "error: no successful $workflow run binds candidate archives to $source_commit" >&2
  exit 1
fi
# GitHub run ids are monotonically increasing. Repeated candidate certification
# for the same immutable source is expected during release recovery; the newest
# successful, unexpired run is the canonical projection. The downstream
# manifest verifier still binds every archive digest, source SHA, policy
# revision, producer run, and attestation before promotion.
printf '%s\n' "$(printf '%s\n' "${matching_run_ids[@]}" | sort -n | tail -n 1)"
