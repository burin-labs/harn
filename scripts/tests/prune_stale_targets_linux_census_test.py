#!/usr/bin/env python3
"""The Linux handle census decides idle entries on a host with root processes.

An unprivileged `lsof` cannot read root's processes, so a census that needed
every process read kept every entry on every Linux host. The /proc census must
remove an idle entry there, and still keep one that a same-user process holds
open, sits in, or locks through Cargo, and one a recent build touched.
"""

import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile


def require(condition, message):
    if not condition:
        raise AssertionError(message)


if platform.system() != "Linux":
    print("prune_stale_targets_linux_census_test: skipped (Linux only)")
    sys.exit(0)

repo = Path(__file__).resolve().parents[2]
script = repo / "scripts/prune_stale_targets.sh"


def root_processes_unreadable():
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/status", encoding="utf-8") as handle:
                uid = next(line for line in handle if line.startswith("Uid:")).split()[2]
            if uid == "0":
                os.listdir(f"/proc/{name}/fd")
        except PermissionError:
            return True
        except (FileNotFoundError, ProcessLookupError, StopIteration):
            continue
    return False


with tempfile.TemporaryDirectory(prefix="harn-gc-linux-census-") as temporary:
    root = Path(temporary)
    storage = root / "storage"
    targets = storage / "harn-target"
    repos = root / "repos"
    repos.mkdir(parents=True)
    env = dict(os.environ, HARN_DEV_SETUP_STORAGE_ROOT=str(storage), HARN_TARGET_GC_ROOTS=str(repos),
               HARN_TARGET_GC_MIN_AGE_SECS="1", HARN_TARGET_GC_KEEP_RECENT="0")
    env.pop("HARN_TARGET_GC_HANDLE_PROBE", None)
    env.pop("HARN_TARGET_GC_PROC_ROOT", None)

    def entry(name):
        path = targets / name
        (path / "debug").mkdir(parents=True)
        (path / "debug" / "artifact").write_text("evidence", encoding="utf-8")
        old = 1_000_000_000
        for item in (path / "debug" / "artifact", path / "debug", path):
            os.utime(item, (old, old))
        return path

    def run(name, extra=None, named=True):
        command = ["bash", str(script), "--dry-run"] + (["--remove-entry", name] if named else [])
        result = subprocess.run(command, env=dict(env, **(extra or {})), capture_output=True, text=True)
        return result.stdout + result.stderr

    # Falsifier: an idle entry on a host whose root processes this user cannot
    # read is removable. The lsof census reported it unmeasured here.
    if os.geteuid() != 0:
        require(root_processes_unreadable(), "precondition: no unreadable root process, the falsifier is vacuous")
    idle = entry("idle")
    out = run("idle")
    require("would remove named by caller: idle" in out, out)
    require("blocked_named_entries=0" in out, out)

    # Negative controls: same-user holders keep the entry.
    # Each holder learns its path from the environment, so only the /proc
    # handle scan can see it: an open descriptor, a mapping with its
    # descriptor closed, and an executable started under another argv[0].
    holders = {
        "held": "import os,sys; f=open(os.environ['HARN_GC_TEST_PATH'],'rb')",
        "mapped": "import mmap,os,sys; f=open(os.environ['HARN_GC_TEST_PATH'],'rb');"
                  " m=mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ); f.close()",
    }
    for name, setup in holders.items():
        held = entry(name)
        holder = subprocess.Popen(
            [sys.executable, "-c", setup + "; print('ready',flush=True); sys.stdin.readline()"],
            env=dict(os.environ, HARN_GC_TEST_PATH=str(held / "debug" / "artifact")),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            require(holder.stdout.readline().strip() == "ready", f"{name} holder did not start")
            out = run(name)
            require(f"keep (live process: {holder.pid})" in out, out)
            require(held.is_dir(), f"a {name} entry was removed")
        finally:
            holder.stdin.write("exit\n")
            holder.stdin.flush()
            holder.communicate(timeout=15)
    running = entry("running")
    binary = running / "debug" / "sleeper"
    binary.write_bytes(Path("/bin/sleep").read_bytes())
    binary.chmod(0o755)
    sleeper = subprocess.Popen(["sleeper", "300"], executable=str(binary))
    try:
        out = run("running")
        require(f"keep (live process: {sleeper.pid})" in out, out)
    finally:
        sleeper.kill()
        sleeper.wait(timeout=15)

    resident = entry("resident")
    sitter = subprocess.Popen([sys.executable, "-c", "import sys; print('ready',flush=True); sys.stdin.readline()"],
                              cwd=resident / "debug", stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        require(sitter.stdout.readline().strip() == "ready", "resident did not start")
        out = run("resident")
        require(f"keep (live process: {sitter.pid})" in out, out)
    finally:
        sitter.stdin.write("exit\n")
        sitter.stdin.flush()
        sitter.communicate(timeout=15)

    # A Cargo lock held by any user keeps the entry, which is how a root build
    # stays visible to an unprivileged census.
    locked = entry("locked")
    lock = locked / "debug" / ".cargo-lock"
    lock.write_text("", encoding="utf-8")
    locker = subprocess.Popen(
        [sys.executable, "-c",
         "import fcntl,os,sys; fd=os.open(os.environ['HARN_GC_TEST_LOCK'], os.O_RDWR);"
         "fcntl.flock(fd, fcntl.LOCK_EX); os.chdir('/'); print('ready',flush=True); sys.stdin.readline()"],
        env=dict(os.environ, HARN_GC_TEST_LOCK=str(lock)), stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        require(locker.stdout.readline().strip() == "ready", "locker did not start")
        out = run("locked", {"HARN_TARGET_GC_PROC_ROOT": str(root / "empty-proc")})
        require("process liveness unavailable" in out, "an empty process table read as measured: " + out)
        fake_proc = root / "lock-proc"
        (fake_proc / "self").mkdir(parents=True)
        (fake_proc / "self" / "status").write_text("Name:\tself\n", encoding="utf-8")
        # A table that hides other users' processes shows no process 1.
        out = run("locked", {"HARN_TARGET_GC_PROC_ROOT": str(fake_proc)})
        require("keep (process ownership unmeasured): locked" in out, "a hidden process table read as measured: " + out)
        (fake_proc / "1").mkdir()
        out = run("locked", {"HARN_TARGET_GC_PROC_ROOT": str(fake_proc)})
        require("keep (live process: cargo-lock)" in out, out)
    finally:
        locker.stdin.write("exit\n")
        locker.stdin.flush()
        locker.communicate(timeout=15)

    # Fail closed: a process of another non-root user that could reach the
    # entry and cannot be inspected leaves the census unmeasured. The same
    # process is out of scope once the entry's ancestors deny it search.
    fake_proc = root / "foreign-proc"
    (fake_proc / "self").mkdir(parents=True)
    (fake_proc / "1").mkdir()
    (fake_proc / "self" / "status").write_text("Name:\tself\n", encoding="utf-8")
    foreign = fake_proc / "4242"
    (foreign / "fd").mkdir(parents=True)
    (foreign / "status").write_text("Name:\tforeign\nUid:\t54321\t54321\t54321\t54321\n"
                                    "Gid:\t54321\t54321\t54321\t54321\nGroups:\t\nCapEff:\t0000000000000000\n",
                                    encoding="utf-8")
    (foreign / "fd").chmod(0)
    try:
        if os.geteuid() != 0:
            root.chmod(0o755)
            out = run("idle", {"HARN_TARGET_GC_PROC_ROOT": str(fake_proc)})
            require("keep (process ownership unmeasured): idle" in out, out)
            root.chmod(0o700)
            out = run("idle", {"HARN_TARGET_GC_PROC_ROOT": str(fake_proc)})
            require("would remove named by caller: idle" in out, out)
    finally:
        (foreign / "fd").chmod(0o755)
        root.chmod(0o700)

    # The idle-age rule still guards automatic removal: beside an idle entry
    # the census releases, a fresh unheld entry stays.
    fresh = targets / "fresh"
    (fresh / "debug").mkdir(parents=True)
    out = run("fresh", {"HARN_TARGET_GC_MIN_AGE_SECS": "3600"}, named=False)
    require("keep (recently active): fresh" in out, out)
    require(any(line.startswith("would remove") and line.split(": ", 1)[-1].startswith("idle ")
                for line in out.splitlines()), out)
    require(not any(line.startswith("would remove") and ": fresh " in line for line in out.splitlines()), out)
    require(fresh.is_dir() and idle.is_dir(), "a dry run removed an entry")

print("prune_stale_targets_linux_census_test: ok")
