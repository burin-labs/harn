#!/usr/bin/env python3
"""Checkout custody protects build outputs independently of Git clone layout."""

import contextlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[2]
COLLECTOR = REPO / "scripts/prune_stale_targets.sh"


class CheckoutOwnershipTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="harn-gc-checkout-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repos = self.root / "repos"
        self.repos.mkdir()
        self.env = dict(os.environ)
        for key in list(self.env):
            if key.startswith("HARN_TARGET_GC_"):
                del self.env[key]
        self.env.update(
            HARN_DEV_SETUP_STORAGE_ROOT=str(self.root / "storage"),
            HARN_TARGET_GC_ROOTS=str(self.repos),
            HARN_TARGET_GC_MIN_AGE_SECS="1",
            HARN_TARGET_GC_MAX_IDLE_SECS="1",
            HARN_TARGET_GC_KEEP_RECENT="0",
            HARN_TARGET_GC_MAX_BYTES="0",
            HARN_TARGET_GC_MIN_FREE_BYTES="0",
        )
        self.probe = self.root / "idle-probe"
        self.probe.write_text("#!/bin/sh\nprintf 'idle\\n'\n", encoding="utf-8")
        self.probe.chmod(0o700)

    def clone(self, name="active", outside=False):
        checkout = (self.root / "outside discovery" if outside else self.repos) / name
        (checkout / ".cargo").mkdir(parents=True)
        subprocess.run(["git", "init", "-q", str(checkout)], check=True)
        (checkout / "source.rs").write_text("known source", encoding="utf-8")
        return checkout

    def target(self, checkout, name="cache", registered=False, family="harn-target"):
        target = self.root / "storage" / family / name
        (target / "debug").mkdir(parents=True)
        (target / "debug" / "artifact").write_text("known artifact", encoding="utf-8")
        (checkout / ".cargo" / "config.toml").write_text(
            f"[build]\ntarget-dir = {json.dumps(str(target))}\n", encoding="utf-8"
        )
        if registered:
            result = subprocess.run(
                ["bash", str(COLLECTOR), "--record-owner", str(target), str(checkout)],
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads((target / ".harn-target-owner.json").read_text()), {
                "schema": "harn.target-owner/v1", "checkout": str(checkout.resolve()),
            })
        for path in [target / "debug" / "artifact", target / "debug", target]:
            os.utime(path, (1_000_000_000, 1_000_000_000))
        return target

    def collect(self, synthetic_idle=False, expected_exit=0):
        env = dict(self.env)
        if synthetic_idle:
            env["HARN_TARGET_GC_HANDLE_PROBE"] = str(self.probe)
        result = subprocess.run(["bash", str(COLLECTOR), "--host-maintenance"],
                                env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, expected_exit, result.stdout + result.stderr)
        return result.stdout + result.stderr

    @contextlib.contextmanager
    def source_holder(self, checkout, open_file=False):
        code = ("import os,sys; "
                + ("f=open(os.environ['HARN_GC_SOURCE_FILE']); " if open_file else "")
                + "print('ready',flush=True); sys.stdin.readline()")
        process = subprocess.Popen(
            [sys.executable, "-c", code], cwd=self.root if open_file else checkout,
            env=dict(os.environ, HARN_GC_SOURCE_FILE=str(checkout / "source.rs")),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
        )
        try:
            self.assertEqual(process.stdout.readline().strip(), "ready")
            if not open_file:
                self.assertEqual(Path(f"/proc/{process.pid}/cwd").resolve(), checkout.resolve())
            yield process
        finally:
            process.communicate("done\n", timeout=10)

    @unittest.skipUnless(platform.system() == "Linux", "canonical /proc custody proof")
    def test_legacy_clone_cwd_survives_then_idle_cache_is_removed(self):
        checkout = self.clone()
        target = self.target(checkout, name="configured-cache")
        orphan = self.target(self.clone("other"), name="idle-control")
        with self.source_holder(checkout) as holder:
            output = self.collect()
            self.assertIsNone(holder.poll())
            self.assertIn(str(holder.pid), output)
            self.assertTrue(target.is_dir(), output)
            self.assertFalse(orphan.exists(), output)
        self.collect()
        self.assertFalse(target.exists())

    @unittest.skipUnless(platform.system() == "Linux", "canonical /proc custody proof")
    def test_registered_clone_outside_discovery_keeps_open_source_reader(self):
        checkout = self.clone(outside=True)
        target = self.target(checkout, registered=True)
        with self.source_holder(checkout, open_file=True) as holder:
            output = self.collect()
            self.assertIn(str(holder.pid), output)
            self.assertTrue(target.is_dir(), output)
        self.collect()
        self.assertFalse(target.exists())

    def test_standalone_manual_lock_keeps_legacy_cache(self):
        checkout = self.clone()
        target = self.target(checkout)
        lock = checkout / ".git" / "locked"
        lock.write_text("reserved for an active task\n", encoding="utf-8")
        self.assertIn("checkout-lock", self.collect(synthetic_idle=True))
        self.assertTrue(target.is_dir())
        lock.unlink()
        self.collect(synthetic_idle=True)
        self.assertFalse(target.exists())

    def test_registered_claim_expiration_is_a_measured_release(self):
        checkout = self.clone(outside=True)
        target = self.target(checkout, registered=True)
        lock = checkout / ".git" / "locked"
        for expiry, expected in [("2099-01-01T00:00:00Z", True), ("2000-01-01T00:00:00Z", False)]:
            lock.write_text("burin-claim " + json.dumps({"v": 1, "owner": "fixture", "expires": expiry}),
                            encoding="utf-8")
            self.collect(synthetic_idle=True)
            self.assertEqual(target.is_dir(), expected)

    def test_linked_worktree_lock_uses_the_same_custody_contract(self):
        main = self.clone(name="main")
        subprocess.run(["git", "-C", str(main), "-c", "commit.gpgsign=false", "-c", "user.name=fixture",
                        "-c", "user.email=fixture@example.invalid", "commit", "-q", "--allow-empty", "-m", "fixture"],
                       check=True)
        checkout = self.root / "linked"
        subprocess.run(["git", "-C", str(main), "worktree", "add", "-q", "-b", "linked", str(checkout)], check=True)
        (checkout / ".cargo").mkdir()
        target = self.target(checkout, name="linked-cache")
        subprocess.run(["git", "-C", str(main), "worktree", "lock", "--reason", "active task", str(checkout)], check=True)
        self.assertIn("checkout-lock", self.collect(synthetic_idle=True))
        self.assertTrue(target.is_dir())
        subprocess.run(["git", "-C", str(main), "worktree", "unlock", str(checkout)], check=True)
        self.collect(synthetic_idle=True)
        self.assertFalse(target.exists())

    def test_registered_owner_that_is_gone_does_not_pin_an_orphan(self):
        checkout = self.clone(outside=True)
        target = self.target(checkout, registered=True)
        shutil.rmtree(checkout)
        self.collect(synthetic_idle=True)
        self.assertFalse(target.exists())

    def test_registered_owner_wins_over_an_unrelated_legacy_basename(self):
        checkout = self.clone(outside=True)
        target = self.target(checkout, registered=True)
        unrelated = self.clone(name="unrelated")
        other_target = self.root / "other-storage" / "harn-target" / target.name
        (unrelated / ".cargo" / "config.toml").write_text(
            f"[build]\ntarget-dir = {json.dumps(str(other_target))}\n", encoding="utf-8"
        )
        (unrelated / ".git" / "locked").write_text("another task's lock\n", encoding="utf-8")
        self.collect(synthetic_idle=True)
        self.assertFalse(target.exists())

    def test_registered_existing_checkout_is_warm_outside_discovery(self):
        checkout = self.clone(outside=True)
        target = self.target(checkout, registered=True)
        self.env["HARN_TARGET_GC_KEEP_RECENT"] = "1"
        self.collect(synthetic_idle=True)
        self.assertTrue(target.is_dir())
        self.env["HARN_TARGET_GC_KEEP_RECENT"] = "0"
        self.collect(synthetic_idle=True)
        self.assertFalse(target.exists())

    def test_unreadable_contract_shapes_keep_the_target_and_report_pending(self):
        for invalid in [[], {"schema": "harn.target-owner/v2", "checkout": str(self.repos)}]:
            with self.subTest(invalid=invalid):
                checkout = self.clone(name=f"shape-{len(list(self.repos.iterdir()))}")
                target = self.target(checkout, name=checkout.name)
                (target / ".harn-target-owner.json").write_text(json.dumps(invalid), encoding="utf-8")
                os.utime(target, (1_000_000_000, 1_000_000_000))
                output = self.collect(synthetic_idle=True, expected_exit=1)
                self.assertTrue(target.is_dir(), output)
                self.assertIn("ownership unmeasured", output)
                summary = next(line for line in output.splitlines() if line.startswith("harn-target GC: policy="))
                fields = dict(token.split("=", 1) for token in summary.split() if "=" in token)
                self.assertGreater(int(fields["pending_candidates"]), 0)

    def test_malformed_claim_is_not_an_expired_claim(self):
        checkout = self.clone()
        target = self.target(checkout)
        (checkout / ".git" / "locked").write_text("burin-claim {broken", encoding="utf-8")
        output = self.collect(synthetic_idle=True, expected_exit=1)
        self.assertTrue(target.is_dir(), output)
        self.assertIn("ownership unmeasured", output)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO metadata is a Unix boundary")
    def test_fifo_metadata_is_refused_without_waiting_for_a_writer(self):
        for kind in ["record", "lock"]:
            with self.subTest(kind=kind):
                checkout = self.clone(name=kind)
                target = self.target(checkout, name=kind)
                metadata = target / ".harn-target-owner.json" if kind == "record" else checkout / ".git" / "locked"
                os.mkfifo(metadata, 0o600)
                os.utime(target, (1_000_000_000, 1_000_000_000))
                output = self.collect(synthetic_idle=True, expected_exit=1)
                self.assertTrue(target.is_dir(), output)
                self.assertIn("ownership unmeasured", output)

    @unittest.skipUnless(platform.system() == "Linux", "canonical /proc custody proof")
    def test_release_cache_uses_the_same_checkout_liveness(self):
        checkout = self.clone(name="release-clone")
        target = self.target(checkout, name=checkout.name, family="release-gate-target")
        with self.source_holder(checkout) as holder:
            output = self.collect()
            self.assertIn(str(holder.pid), output)
            self.assertTrue(target.is_dir(), output)
        self.collect()
        self.assertFalse(target.exists())


if __name__ == "__main__":
    unittest.main()
