#!/usr/bin/env bash
# Verify that Release smoke has succeeded for a published tag.
#
# A published release is not declarable-ready until the cross-platform
# Release smoke workflow has actually run and passed against that tag's
# artifacts (see harn#5034). This script is the release-verification
# checklist gate for that requirement.
set -euo pipefail

usage() {
  cat <<'EOF' >&2
usage: scripts/check_release_smoke.sh vX.Y.Z

Exit codes:
  0  a successful Release smoke run covers the tag
  1  no successful covering run found (or gh/query failure)
  2  usage error
EOF
}

tag="${1:-}"
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  usage
  exit 2
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "error: gh is required to query Release smoke runs" >&2
  exit 1
fi

repo="${GITHUB_REPOSITORY:-}"
if [[ -z "$repo" ]]; then
  repo="$(gh repo view --json nameWithOwner --jq '.nameWithOwner')"
fi

runs_json="$(
  gh run list \
    --repo "$repo" \
    --workflow 'Release smoke' \
    --limit 100 \
    --json databaseId,conclusion,event,displayTitle,headBranch,url,createdAt
)"

export CHECK_RELEASE_SMOKE_REPO="$repo"
python3 - "$tag" "$runs_json" <<'PY'
import json
import os
import subprocess
import sys

tag = sys.argv[1]
runs = json.loads(sys.argv[2])
repo = os.environ["CHECK_RELEASE_SMOKE_REPO"]

def title_or_branch_covers(run: dict) -> bool:
    if run.get("conclusion") != "success":
        return False
    title = str(run.get("displayTitle") or "")
    branch = str(run.get("headBranch") or "")
    event = str(run.get("event") or "")
    if f"({tag})" in title:
        return True
    if branch == tag:
        return True
    if branch == f"release/{tag}" or branch.startswith(f"release-attempt/{tag}/"):
        return event in {"workflow_run", "workflow_dispatch", "release"}
    return False

def log_covers(run_id: int) -> bool:
    # workflow_run smokes execute from the default branch, so headBranch/head_sha
    # do not name the release tag. Fall back to the resolve/checkout markers that
    # artifact mode always prints for the candidate tag.
    try:
        log = subprocess.check_output(
            ["gh", "run", "view", str(run_id), "--repo", repo, "--log"],
            text=True,
            stderr=subprocess.DEVNULL,
        )
    except subprocess.CalledProcessError:
        return False
    markers = (
        f"All release assets for {tag} are available",
        f"TAG: {tag}",
        f"ref: {tag}",
        f"refs/tags/{tag}",
    )
    return any(marker in log for marker in markers)

matches = [run for run in runs if title_or_branch_covers(run)]
if not matches:
    candidates = [
        run
        for run in runs
        if run.get("conclusion") == "success"
        and run.get("event") in {"workflow_run", "workflow_dispatch", "release"}
    ]
    # Newest first; cap log scans so a missing tag fails closed quickly.
    candidates.sort(key=lambda run: run.get("createdAt") or "", reverse=True)
    for run in candidates[:40]:
        if log_covers(int(run["databaseId"])):
            matches.append(run)
            break

if not matches:
    print(
        f"error: no successful Release smoke run found for {tag}\n"
        f"  checklist: a published release is not declarable-ready until\n"
        f"  Release smoke has passed against its artifacts (harn#5034).\n"
        f"  Dispatch manually with:\n"
        f"    gh workflow run 'Release smoke' -f tag={tag}",
        file=sys.stderr,
    )
    raise SystemExit(1)

best = sorted(matches, key=lambda run: run.get("createdAt") or "", reverse=True)[0]
print(
    f"check_release_smoke: {tag} covered by run {best['databaseId']} "
    f"({best.get('event')}, {best.get('createdAt')})"
)
print(best.get("url") or "")
PY
