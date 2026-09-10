#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
python3 - "$repo_root/scripts/ci/wait_for_run_artifacts.sh" <<'PY'
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    executable = root / "gh"
    executable.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
assert sys.argv[1] == "api"
assert sys.argv[3:] == ["--paginate", "--slurp"]
scenario = os.environ["FIXTURE_SCENARIO"]
path = sys.argv[2]
kind = "artifacts" if "/artifacts?" in path else "jobs"
if kind == "jobs":
    assert "/attempts/2/jobs?per_page=100" in path
count_file = Path(os.environ["FIXTURE_ROOT"]) / kind
count = int(count_file.read_text()) + 1 if count_file.exists() else 1
count_file.write_text(str(count))
if scenario == "api_error":
    sys.exit(1)
if kind == "artifacts":
    ready = scenario == "early" or (scenario == "delayed" and count >= 3) or (scenario == "race" and count >= 2)
    if scenario == "malformed_artifact":
        print(json.dumps([{"artifacts": [{"name":"harn-cli.tar.zst", "expired":"false"}]}]))
    elif scenario == "multiple":
        print(json.dumps([{"artifacts": [{"name":"harn-cli.tar.zst", "expired":False}]}, {"artifacts": [{"name":"harn-security.tar.zst", "expired":False}] if count >= 3 else []}]))
    elif scenario == "expired":
        print(json.dumps([{"artifacts": [{"name":"harn-cli.tar.zst", "expired":True}]}]))
    else:
        print(json.dumps([{"artifacts": []}, {"artifacts": [{"name":"harn-cli.tar.zst", "expired":False}] if ready else []}]))
else:
    conclusion = scenario.removeprefix("terminal_") if scenario.startswith("terminal_") else "success"
    job = {"id": 12, "name": "Rust workspace tests", "status":"completed", "conclusion":conclusion}
    if scenario in ("delayed", "multiple", "expired", "malformed_artifact", "running"):
        job.update(status="in_progress", conclusion=None)
    if scenario == "unknown_status":
        job.update(status="future_terminal")
    if scenario == "unknown_conclusion":
        job.update(conclusion="future_success")
    jobs = [job]
    if scenario == "absent":
        job.update(name="unrelated completed job")
    if scenario == "duplicate":
        jobs.append(dict(job, id=13))
    if scenario == "malformed_job":
        job.pop("id")
    if scenario == "empty_pages":
        print("[]")
    else:
        print(json.dumps([{"jobs": [{"id":1,"name":"other","status":"completed"}]}, {"jobs":jobs}]))
''')
    executable.chmod(0o700)

    def run(scenario, artifacts=("harn-cli.tar.zst",)):
        for name in ("artifacts", "jobs"):
            (root / name).unlink(missing_ok=True)
        env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                   FIXTURE_SCENARIO=scenario, FIXTURE_ROOT=str(root),
                   GITHUB_REPOSITORY="burin-labs/harn", GITHUB_RUN_ID="123",
                   GITHUB_RUN_ATTEMPT="2", HARN_ARTIFACT_PRODUCER_JOB="Rust workspace tests",
                   HARN_ARTIFACT_WAIT_MAX_ATTEMPTS="3", HARN_ARTIFACT_WAIT_INTERVAL_SECONDS="0")
        result = subprocess.run([sys.argv[1], *artifacts], env=env,
                                capture_output=True, text=True, timeout=10)
        counts = {name: int((root / name).read_text()) if (root / name).exists() else 0
                  for name in ("artifacts", "jobs")}
        return result, counts

    for scenario, reads in (("early",1), ("delayed",3), ("race",2)):
        result, counts = run(scenario)
        assert result.returncode == 0, (scenario, result.stderr)
        assert "run artifacts ready: harn-cli.tar.zst" in result.stdout
        assert counts["artifacts"] == reads, (scenario, counts)
        if scenario == "early":
            assert counts["jobs"] == 0, counts

    result, counts = run("multiple", ("harn-cli.tar.zst", "harn-security.tar.zst"))
    assert result.returncode == 0, result.stderr
    assert "run artifacts ready: harn-cli.tar.zst harn-security.tar.zst" in result.stdout
    assert "attempt 1/3): harn-security.tar.zst" in result.stdout
    assert counts == {"artifacts": 3, "jobs": 2}, counts

    for conclusion in ("failure", "cancelled", "skipped", "success", "timed_out"):
        result, counts = run(f"terminal_{conclusion}")
        assert result.returncode == 1, (conclusion, result)
        assert f"producer 'Rust workspace tests' completed ({conclusion})" in result.stderr
        assert "harn-cli.tar.zst" in result.stderr
        assert counts == {"artifacts": 2, "jobs": 1}, counts

    for scenario in ("api_error", "absent", "duplicate", "malformed_job", "empty_pages",
                     "unknown_status", "unknown_conclusion", "malformed_artifact", "running", "expired"):
        result, counts = run(scenario)
        assert result.returncode == 1, (scenario, result)
        assert "timed out waiting for run artifacts after 3 attempts: harn-cli.tar.zst" in result.stderr, (scenario, result.stderr)
        assert counts == {"artifacts": 3, "jobs": 3}, (scenario, counts)
    print("ci_wait_for_run_artifacts_test: 19 scenarios passed")
PY
