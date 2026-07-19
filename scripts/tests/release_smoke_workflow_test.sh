#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo_root/.github/workflows/release-smoke.yml"

python3 - "$workflow" <<'PY'
from pathlib import Path
import sys
import yaml

workflow_path = Path(sys.argv[1])
text = workflow_path.read_text()
workflow = yaml.safe_load(text)

# PyYAML still treats the unquoted GitHub Actions `on` key as YAML 1.1 bool.
on_block = workflow.get("on", workflow.get(True, {}))
failures = []

push = on_block.get("push")
if push and "tags" in push:
    failures.append("release-smoke must not run directly on tag pushes")

workflow_run = on_block.get("workflow_run")
if workflow_run is None:
    failures.append("release-smoke must be triggered by build-release-binaries completion")
else:
    if workflow_run.get("workflows") != ["Build release binaries"]:
        failures.append("workflow_run must listen only to Build release binaries")
    if workflow_run.get("types") != ["completed"]:
        failures.append("workflow_run must trigger only on completed runs")

for forbidden in (
    "HARN_RELEASE_ASSET_WAIT_SECONDS",
    "sleep 30",
    "while true",
    "Timed out waiting for release assets",
):
    if forbidden in text:
        failures.append(f"release-smoke must not hold a runner with polling loop token: {forbidden}")

jobs = workflow.get("jobs", {})
resolve = jobs.get("resolve", {})
resolve_if = str(resolve.get("if", ""))
for required in (
    "github.event.workflow_run.conclusion == 'success'",
    "github.event.workflow_run.event == 'push'",
    "startsWith(github.event.workflow_run.head_branch, 'v')",
):
    if required not in resolve_if:
        failures.append(f"resolve job must gate workflow_run releases with: {required}")

timeout = resolve.get("timeout-minutes")
if not isinstance(timeout, int) or timeout > 10:
    failures.append("resolve job must stay short because it validates assets once instead of waiting")

resolve_step = None
for step in resolve.get("steps", []):
    if step.get("id") == "resolve":
        resolve_step = step
        break
if resolve_step is None:
    failures.append("resolve job must keep an id=resolve step")
else:
    run = str(resolve_step.get("run", ""))
    if run.count("gh release view") != 1:
        failures.append("resolve step must inspect release assets exactly once")
    if "Missing release assets" not in run:
        failures.append("resolve step must fail fast when finalized assets are absent")
    for asset in ("SHA256SUMS", "release-assets.json"):
        if asset not in run:
            failures.append(f"resolve step must require finalized release asset {asset}")

smoke = jobs.get("smoke", {})
if str(smoke.get("if", "")).strip() != "needs.resolve.result == 'success'":
    failures.append("smoke matrix must run only after successful input resolution")
checkout = None
for step in smoke.get("steps", []):
    if str(step.get("uses", "")).startswith("actions/checkout@"):
        checkout = step
        break
if checkout is None:
    failures.append("smoke matrix must check out repository scripts")
else:
    checkout_ref = str(checkout.get("with", {}).get("ref", ""))
    if checkout_ref != "${{ needs.resolve.outputs.tag || github.sha }}":
        failures.append("artifact smoke must check out the release tag, while source smoke keeps github.sha")

run_smoke = None
for step in smoke.get("steps", []):
    if step.get("name") == "Run release smoke":
        run_smoke = str(step.get("run", ""))
        break
if run_smoke is None:
    failures.append("smoke matrix must keep an attributable Run release smoke step")
else:
    for required in (
        '"$HARN_BINARY" run --no-sandbox scripts/release_smoke.harn',
        '--candidate "$HARN_BINARY"',
        '--step-timeout-ms 120000',
    ):
        if required not in run_smoke:
            failures.append(f"release smoke must invoke the exact candidate through Harn: {required}")
    for forbidden in ("release_smoke.sh", "cargo build", "sleep ", "while ", "kill ", "taskkill"):
        if forbidden in run_smoke:
            failures.append(f"post-bootstrap release smoke must not own shell lifecycle: {forbidden}")

if failures:
    for failure in failures:
        print(f"release_smoke_workflow_test: {failure}", file=sys.stderr)
    raise SystemExit(1)

print("release_smoke_workflow_test: release smoke waits for finalized build-release-binaries output")
PY
