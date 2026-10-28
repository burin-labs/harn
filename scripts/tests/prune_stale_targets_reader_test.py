#!/usr/bin/env python3
"""A target with an open artifact stays until its reader exits."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile


def require(condition, message):
    if not condition:
        raise AssertionError(message)


repo = Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix="harn-gc-reader-") as temporary:
    root = Path(temporary)
    target = root / "storage" / "harn-target" / "reader"
    target.mkdir(parents=True)
    artifact = target / "artifact"
    artifact.write_text("evidence", encoding="utf-8")
    repos = root / "repos"
    repos.mkdir()
    env = dict(os.environ, HARN_DEV_SETUP_STORAGE_ROOT=str(root / "storage"),
               HARN_TARGET_GC_ROOTS=str(repos), HARN_TARGET_GC_MIN_AGE_SECS="1",
               HARN_TARGET_GC_KEEP_RECENT="0", HARN_GC_READER_FILE=str(artifact))
    env.pop("HARN_TARGET_GC_HANDLE_PROBE", None)
    holder_code = (
        "import os,sys; f=open(os.environ['HARN_GC_READER_FILE'],'rb'); "
        "print('ready',flush=True); sys.stdin.readline(); f.close()"
    )
    holder = subprocess.Popen([sys.executable, "-c", holder_code], env=env,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        require(holder.stdout is not None and holder.stdout.readline().strip() == "ready", "reader did not start")
        observed = subprocess.run(["lsof", "-t", "--", str(artifact)],
                                  capture_output=True, text=True, check=True)
        require(str(holder.pid) in observed.stdout.splitlines(), "reader was not measured")
        census = subprocess.run(["lsof", "-n", "-Fpfn"], capture_output=True, text=True)
        fully_measured = (census.returncode == 0 and bool(census.stdout) and not census.stderr
                          and "fNOFD\n" not in census.stdout
                          and "Permission denied)" not in census.stdout
                          and "Operation not permitted)" not in census.stdout)
        command = ["bash", str(repo / "scripts/prune_stale_targets.sh"),
                   "--dry-run", "--remove-entry", "reader"]
        busy = subprocess.run(command, env=env, capture_output=True, text=True, check=True)
        expected_busy = "keep (live process:" if fully_measured else "keep (process ownership unmeasured): reader"
        require(expected_busy in busy.stdout, busy.stdout + busy.stderr)
        require("blocked_named_entries=1" in busy.stdout, busy.stdout + busy.stderr)
        require(target.is_dir(), "dry run removed the target")
    finally:
        require(holder.stdin is not None, "reader input is unavailable")
        holder.stdin.write("exit\n")
        holder.stdin.flush()
        holder.communicate(timeout=15)
    idle = subprocess.run(command, env=env, capture_output=True, text=True, check=True)
    if fully_measured:
        require("would remove named by caller: reader" in idle.stdout, idle.stdout + idle.stderr)
        require("blocked_named_entries=0" in idle.stdout, idle.stdout + idle.stderr)
    else:
        require("keep (process ownership unmeasured): reader" in idle.stdout, idle.stdout + idle.stderr)
        require("blocked_named_entries=1" in idle.stdout, idle.stdout + idle.stderr)
    with artifact.open("rb"):
        ancestor = subprocess.run(command, env=env, capture_output=True, text=True, check=True)
        require(expected_busy in ancestor.stdout, ancestor.stdout + ancestor.stderr)
        require("blocked_named_entries=1" in ancestor.stdout, ancestor.stdout + ancestor.stderr)
    fake_bin = root / "fake-bin"
    fake_bin.mkdir()
    fake_lsof = fake_bin / "lsof"
    fake_lsof.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
    fake_lsof.chmod(0o755)
    unreadable_env = dict(env, PATH=str(fake_bin) + os.pathsep + env["PATH"])
    unreadable = subprocess.run(command, env=unreadable_env, capture_output=True, text=True)
    require("keep (process ownership unmeasured): reader" in unreadable.stdout, unreadable.stdout + unreadable.stderr)
    require("blocked_named_entries=1" in unreadable.stdout, unreadable.stdout + unreadable.stderr)
    require(target.is_dir(), "unreadable census removed the target")
    fake_lsof.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    empty = subprocess.run(command, env=unreadable_env, capture_output=True, text=True)
    require("keep (process ownership unmeasured): reader" in empty.stdout, empty.stdout + empty.stderr)
    require("blocked_named_entries=1" in empty.stdout, empty.stdout + empty.stderr)
    require(target.is_dir(), "empty census removed the target")
    fake_lsof.write_text("#!/bin/sh\nprintf 'p999\\nfNOFD\\nn/proc/999/fd (opendir: Permission denied)\\n'\n", encoding="utf-8")
    partial = subprocess.run(command, env=unreadable_env, capture_output=True, text=True)
    require("keep (process ownership unmeasured): reader" in partial.stdout, partial.stdout + partial.stderr)
    require("blocked_named_entries=1" in partial.stdout, partial.stdout + partial.stderr)
    require(target.is_dir(), "partial census removed the target")
    fake_lsof.write_text(
        '#!/bin/sh\nif [ -e "$HARN_GC_LSOF_CALLED" ]; then exit 1; fi\n'
        ': > "$HARN_GC_LSOF_CALLED"\nprintf "p$$\\nfcwd\\nn/\\n"\n',
        encoding="utf-8",
    )
    late_failure_env = dict(unreadable_env, HARN_GC_LSOF_CALLED=str(root / "lsof-called"))
    late_failure = subprocess.run(command, env=late_failure_env, capture_output=True, text=True)
    require("process ownership unmeasured before removal" in late_failure.stderr, late_failure.stdout + late_failure.stderr)
    require("blocked_named_entries=1" in late_failure.stdout, late_failure.stdout + late_failure.stderr)
    require(target.is_dir(), "late census failure removed the target")
    fake_ps = fake_bin / "ps"
    fake_ps.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
    fake_ps.chmod(0o755)
    degraded = subprocess.run(command, env=unreadable_env, capture_output=True, text=True)
    require("process liveness unavailable" in degraded.stdout, degraded.stdout + degraded.stderr)
    require("keep (process ownership unmeasured): reader" in degraded.stdout, degraded.stdout + degraded.stderr)
    require("blocked_named_entries=1" in degraded.stdout, degraded.stdout + degraded.stderr)
    require(target.is_dir(), "unavailable process census removed the target")

    # A host can supply its existing complete handle census. Its tri-state
    # contract must retain on busy, unknown, malformed output, or failure.
    host_probe = root / "host-probe"
    host_probe.write_text('#!/bin/sh\nprintf "%s\\n" "$HARN_GC_TEST_VERDICT"\n', encoding="utf-8")
    host_probe.chmod(0o755)
    host_env = dict(env, HARN_TARGET_GC_HANDLE_PROBE=str(host_probe))
    for verdict in ("busy", "unknown", "", "unexpected"):
        result = subprocess.run(command, env=dict(host_env, HARN_GC_TEST_VERDICT=verdict),
                                capture_output=True, text=True, check=True)
        require("blocked_named_entries=1" in result.stdout, result.stdout + result.stderr)
        expected = "keep (live process: host-handle-probe)" if verdict == "busy" else "keep (process ownership unmeasured): reader"
        require(expected in result.stdout, result.stdout + result.stderr)
        require(target.is_dir(), f"host probe {verdict!r} removed the target")
    idle_host = subprocess.run(command, env=dict(host_env, HARN_GC_TEST_VERDICT="idle"),
                               capture_output=True, text=True, check=True)
    require("would remove named by caller: reader" in idle_host.stdout, idle_host.stdout + idle_host.stderr)
    require("blocked_named_entries=0" in idle_host.stdout, idle_host.stdout + idle_host.stderr)
    host_probe.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
    failed_host = subprocess.run(command, env=host_env, capture_output=True, text=True, check=True)
    require("blocked_named_entries=1" in failed_host.stdout, failed_host.stdout + failed_host.stderr)
    require(target.is_dir(), "failed host probe removed the target")
    relative_host = subprocess.run(command, env=dict(env, HARN_TARGET_GC_HANDLE_PROBE="relative"),
                                   capture_output=True, text=True, check=True)
    require("configured handle probe unavailable" in relative_host.stdout, relative_host.stdout + relative_host.stderr)
    require("blocked_named_entries=1" in relative_host.stdout, relative_host.stdout + relative_host.stderr)

    actual = subprocess.run([part for part in command if part != "--dry-run"],
                            env=env, capture_output=True, text=True)
    if fully_measured:
        require(actual.returncode == 0, actual.stdout + actual.stderr)
        require(not target.exists(), actual.stdout + actual.stderr)
    else:
        require(actual.returncode != 0, actual.stdout + actual.stderr)
        require("blocked_named_entries=1" in actual.stdout, actual.stdout + actual.stderr)
        require(target.is_dir(), "unmeasured host removed the target")
print("prune_stale_targets_reader_test: "
      + ("external and ancestor readers kept; closed target removed; "
         if fully_measured else "host census unmeasured, target kept; ")
      + "failed, empty, partial and late censuses kept")
