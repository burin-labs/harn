#!/usr/bin/env python3
"""Exercise retained receipts and direct cancellation through the actual wrapper."""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[2]
FILES = [
    "scripts/ci/run_thread_parity.sh",
    "scripts/ci/run_rust_test_lane.sh",
    "scripts/harn_test_env.sh",
    "scripts/bootstrap-harn.sh",
    "scripts/ci/summarize_nextest_receipt.harn",
    "scripts/ci/nextest_inventory.harn",
]
PRODUCER = r'''#!/usr/bin/env python3
import json, os, signal, socket, sys
from pathlib import Path
names = ["completed", "pending", "queued"]
mode = os.environ["PARITY_FIXTURE_MODE"]
if sys.argv[1:3] == ["nextest", "list"]:
    if mode == "inventory-error":
        print("intentional inventory refusal", file=sys.stderr)
        sys.exit(70)
    print(json.dumps({"test-count": 3, "rust-suites": {"probe::unit": {
        "package-name": "probe", "binary-name": "unit", "status": "listed",
        "testcases": {name: {"ignored": False, "filter-match": {"status": "matches"}}
                      for name in names}}}}))
    sys.exit(0)
assert sys.argv[1:3] == ["nextest", "run"]
Path(os.environ["PARITY_PID_RECORD"]).write_text(str(os.getpid()))
for index, name in enumerate(names):
    print(json.dumps({"type": "test", "event": "started", "name": "probe::unit$" + name}), flush=True)
    if mode in ("interrupted", "held-cleanup") and index == 1:
        ready = socket.socket(socket.AF_UNIX)
        ready.connect(os.environ["PARITY_SOCKET"])
        if mode == "held-cleanup":
            def cleanup(_signal, _frame):
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                ready.sendall(b"cleanup\n")
                ready.recv(1)
                sys.exit(143)
            signal.signal(signal.SIGTERM, cleanup)
        ready.sendall(b"ready\n")
        ready.recv(1)
        raise AssertionError("interrupted producer must not resume")
    event = "failed" if mode == "failed" and index == 1 else "ok"
    print(json.dumps({"type": "test", "event": event, "name": "probe::unit$" + name}), flush=True)
    if "--status-level=pass" in sys.argv:
        print(f"PASS ({index + 1}/3) probe::unit {name}", file=sys.stderr, flush=True)
sys.exit(100 if mode == "failed" else 0)
'''

REPORTER = r'''#!/usr/bin/env python3
import os, socket, subprocess, sys
status = subprocess.call([os.environ["PARITY_REAL_REPORTER"], *sys.argv[1:]])
with socket.socket(socket.AF_UNIX) as receipt:
    receipt.connect(os.environ["PARITY_RECEIPT_SOCKET"])
    receipt.sendall(b"written\n")
sys.exit(status)
'''


def run_case(case: str, binary: str) -> None:
    with tempfile.TemporaryDirectory(prefix="parity-", dir="/tmp") as temporary:
        scratch = Path(temporary)
        repository = scratch / "repo"
        for relative in FILES:
            target = repository / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, target)
            target.chmod(0o755)
        tools = scratch / "bin"
        tools.mkdir()
        cargo = tools / "cargo-nextest"
        cargo.write_text(PRODUCER)
        cargo.chmod(0o755)
        runner = scratch / "runner"
        runner.mkdir()
        output = scratch / "github-output"
        output.touch()
        pid_record = scratch / "producer-pid"
        env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                   HARN_BIN=binary, RUNNER_TEMP=str(runner), GITHUB_OUTPUT=str(output),
                   GITHUB_STEP_SUMMARY=str(scratch / "summary.md"),
                   PARITY_FIXTURE_MODE="interrupted" if case in ("interrupted-int", "lane-int") else "held-cleanup" if case == "interrupted-cleanup" else case,
                   PARITY_PID_RECORD=str(pid_record))
        env.pop("RUST_TEST_JOB_LIMIT_MINUTES", None)
        env.pop("RUST_TEST_FEEDBACK_BUDGET_SECONDS", None)
        if case == "reporter-error":
            env["HARN_BIN"] = str(scratch / "absent-reporter")
        if case == "reporter-run-error":
            reporter = tools / "failed-reporter"
            reporter.write_text("#!/usr/bin/env bash\nexit 74\n")
            reporter.chmod(0o755)
            env["HARN_BIN"] = str(reporter)
        bootstrap = repository / "scripts/bootstrap-harn.sh"
        bootstrap.write_text("#!/usr/bin/env bash\nprintf invoked > bootstrap-invoked\nexit 88\n")
        listener = None
        receipt_listener = None
        if case in ("interrupted", "interrupted-int", "lane-int", "interrupted-cleanup"):
            listener = socket.socket(socket.AF_UNIX)
            listener.settimeout(60)
            listener.bind(str(scratch / "ready.sock"))
            listener.listen(1)
            env["PARITY_SOCKET"] = str(scratch / "ready.sock")
        if case == "interrupted-cleanup":
            receipt_listener = socket.socket(socket.AF_UNIX)
            receipt_listener.settimeout(60)
            receipt_listener.bind(str(scratch / "receipt.sock"))
            receipt_listener.listen(2)
            reporter = tools / "receipt-reporter"
            reporter.write_text(REPORTER)
            reporter.chmod(0o755)
            env.update(HARN_BIN=str(reporter), PARITY_REAL_REPORTER=binary,
                       PARITY_RECEIPT_SOCKET=str(scratch / "receipt.sock"))
        process = None
        try:
            with (scratch / "stdout.log").open("w") as stdout, (scratch / "stderr.log").open("w") as stderr:
                command = ["bash", str(repository / FILES[0]), "0" if case == "invalid" else "1"]
                if case == "lane-int":
                    command = ["bash", str(repository / FILES[1]), "cargo-nextest", "nextest", "run", "--status-level=pass"]
                    env["RUST_TEST_STDOUT_PATH"] = str(scratch / "lane-events.jsonl")
                process = subprocess.Popen(command,
                                           cwd=repository, env=env, stdout=stdout, stderr=stderr,
                                           start_new_session=True)
                if listener is not None:
                    connection, _ = listener.accept()
                    connection.settimeout(60)
                    with connection, connection.makefile("rb") as ready:
                        assert ready.readline() == b"ready\n"
                        process.send_signal(signal.SIGINT if case in ("interrupted-int", "lane-int") else signal.SIGTERM)
                        if case == "interrupted-cleanup":
                            assert ready.readline() == b"cleanup\n", "producer cleanup was not reached"
                            reported, _ = receipt_listener.accept()
                            reported.settimeout(60)
                            with reported, reported.makefile("rb") as notification:
                                assert notification.readline() == b"written\n"
                            evidence = dict(line.split("=", 1) for line in output.read_text().splitlines())["evidence_dir"]
                            partial = json.loads((Path(evidence) / "receipt.json").read_text())
                            assert partial["receipt"]["passed"] == 1
                            assert partial["receipt"]["pending"] == ["probe::unit$pending"]
                            assert partial["status"] == 143 and process.poll() is None
                            os.kill(int(pid_record.read_text()), 0)
                            connection.sendall(b"r")
                        status = process.wait(timeout=60)
                else:
                    status = process.wait(timeout=60)
            if case in ("invalid", "reporter-error"):
                expected = "usage:" if case == "invalid" else "reason=reporter-unavailable"
                assert status == 2 and expected in (scratch / "stderr.log").read_text()
                assert not pid_record.exists() and output.read_text() == ""
                print(f"thread-parity wrapper case={case} passed")
                return
            if case == "lane-int":
                assert status == 130
                assert "Rust test resources after SIGINT" in (scratch / "stdout.log").read_text()
                raw = [json.loads(line) for line in (scratch / "lane-events.jsonl").read_text().splitlines()]
                assert sum(event["event"] == "ok" for event in raw) == 1
                assert sum(event["event"] == "started" for event in raw) == 2
                try:
                    os.kill(int(pid_record.read_text()), 0)
                except ProcessLookupError:
                    pass
                else:
                    raise AssertionError("the direct lane wrapper retained its producer")
                print("thread-parity wrapper case=lane-int passed")
                return
            evidence = dict(line.split("=", 1) for line in output.read_text().splitlines()).get("evidence_dir")
            assert evidence is not None, "the actual wrapper did not publish its evidence directory"
            folder = Path(evidence)
            assert folder.is_dir(), "the actual wrapper deleted its partial evidence"
            stdout_text = (scratch / "stdout.log").read_text()
            stderr_text = (scratch / "stderr.log").read_text()
            assert not (repository / "bootstrap-invoked").exists(), "receipt reporting invoked the installer"
            if case == "inventory-error":
                assert status == 70 and "reason=inventory-error" in stderr_text
            elif case == "reporter-run-error":
                assert status == 74, "a reporter failure changed a run into success"
                assert (folder / "inventory.json").stat().st_size > 0
                assert (folder / "events.jsonl").stat().st_size > 0
            else:
                result = json.loads((folder / "receipt.json").read_text())
                receipt = result["receipt"]
                assert receipt["selected"] == 3
                assert "PASS (1/3)" in stderr_text, "native progress did not survive the stdout redirect"
                if case in ("interrupted", "interrupted-int", "interrupted-cleanup"):
                    expected_status = 130 if case == "interrupted-int" else 143
                    assert status == expected_status and result["status"] == expected_status
                    assert receipt["run"] == receipt["passed"] == 1 and receipt["failed"] == 0
                    assert receipt["started"] == 2
                    assert receipt["pending"] == ["probe::unit$pending"]
                    assert receipt["missing"] == ["probe::unit$pending", "probe::unit$queued"]
                    assert receipt["failed_names"] == []
                elif case == "failed":
                    assert status == 100 and receipt["failed"] == 1
                    assert receipt["failed_names"] == ["probe::unit$pending"]
                    assert receipt["pending"] == receipt["missing"] == []
                else:
                    assert status == 0 and receipt["run"] == receipt["passed"] == 3
                    assert receipt["failed"] == 0
                    assert receipt["pending"] == receipt["missing"] == receipt["failed_names"] == []
                    assert "PASS (3/3)" in stderr_text
                assert "thread_parity_receipt reason=" in stdout_text
            if pid_record.exists():
                try:
                    os.kill(int(pid_record.read_text()), 0)
                except ProcessLookupError:
                    pass
                else:
                    raise AssertionError("the wrapper retained its measured producer process")
        except BaseException:
            print((scratch / "stdout.log").read_text())
            print((scratch / "stderr.log").read_text())
            raise
        finally:
            if process is not None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
            if listener is not None:
                listener.close()
            if receipt_listener is not None:
                receipt_listener.close()
    print(f"thread-parity wrapper case={case} passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", choices=["complete", "interrupted", "interrupted-int", "interrupted-cleanup", "lane-int", "failed", "inventory-error", "reporter-error", "reporter-run-error", "invalid"])
    parser.add_argument("--source-root", type=Path, default=ROOT)
    arguments = parser.parse_args()
    ROOT = arguments.source_root
    harn = os.environ.get("HARN_BIN") or os.environ.get("HARN_EXT_BOOTSTRAP_INSTALLER") or shutil.which("harn")
    assert harn, "a qualified Harn binary is required"
    for selected in ([arguments.case] if arguments.case else ["complete", "interrupted", "interrupted-int", "interrupted-cleanup", "lane-int", "failed", "inventory-error", "reporter-error", "reporter-run-error", "invalid"]):
        run_case(selected, harn)
