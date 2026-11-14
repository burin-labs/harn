"""Real filesystem and process controls for partial target-cache reclamation."""

import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


REPO = Path(__file__).resolve().parents[2]
COLLECTOR = Path(os.environ.get("TARGET_GC_TEST_COLLECTOR", REPO / "scripts/prune_stale_targets.sh"))
spec = importlib.util.spec_from_file_location("target_gc_incremental", REPO / "scripts/lib/target_gc_incremental.py")
filesystem = importlib.util.module_from_spec(spec)
spec.loader.exec_module(filesystem)


class IncrementalCollector(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="harn-incremental-gc-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.storage = self.root / "storage"
        self.targets = self.storage / "harn-target"
        self.target = self.targets / "selected"
        self.profile = self.target / "debug"
        self.make_profile(self.profile)
        self.other = self.targets / "other"
        self.make_profile(self.other / "debug")
        self.repos = self.root / "repos"
        self.repos.mkdir()
        self.receipt = self.storage / "prune-stale-targets.last-success"
        self.receipt.write_text("prior-full-sweep\nprior-policy\n")
        self.probe = self.root / "probe.sh"
        self.probe.write_text("#!/bin/sh\nprintf 'idle\\n'\n")
        self.probe.chmod(0o700)
        self.env = dict(os.environ, HARN_DEV_SETUP_STORAGE_ROOT=str(self.storage),
                        HARN_TARGET_GC_ROOTS=str(self.repos),
                        HARN_TARGET_GC_HANDLE_PROBE=str(self.probe),
                        HARN_TARGET_GC_MIN_AGE_SECS="0", HARN_TARGET_GC_KEEP_RECENT="0",
                        HARN_TARGET_GC_MAX_IDLE_SECS="0", HARN_TARGET_GC_MAX_BYTES="1")
        self.env.pop("HARN_TARGET_GC_PROTECT", None)

    @staticmethod
    def make_profile(profile):
        (profile / "incremental").mkdir(parents=True)
        (profile / "incremental/cache").write_bytes(b"i" * 8192)
        (profile / ".cargo-lock").touch()
        (profile / "deps").mkdir()
        (profile / "deps/library.rlib").write_bytes(b"linked dependency")
        (profile / "harn").write_bytes(b"qualified executable")

    def collect(self, *args, env=None):
        result = subprocess.run(["bash", str(COLLECTOR), *args], env=env or self.env,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        receipts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
        summaries = [line for line in result.stdout.splitlines() if line.startswith("harn-target GC: policy=")]
        self.assertTrue(summaries, result.stdout + result.stderr)
        summary = dict(re.findall(r"([a-z_]+)=([^ ]+)", summaries[-1]))
        return result, receipts, summary

    def assert_preserved(self):
        self.assertTrue(self.target.is_dir())
        self.assertEqual((self.profile / "deps/library.rlib").read_bytes(), b"linked dependency")
        self.assertEqual((self.profile / "harn").read_bytes(), b"qualified executable")
        self.assertEqual((self.other / "debug/incremental/cache").read_bytes(), b"i" * 8192)
        self.assertEqual(self.receipt.read_text(), "prior-full-sweep\nprior-policy\n")

    def test_exact_idle_reclaim_preserves_warm_output_and_other_entries(self):
        _, receipts, summary = self.collect("--prune-incremental-entry", "selected")
        self.assertEqual([r["status"] for r in receipts], ["staged", "reclaimed"])
        self.assertEqual(receipts[-1]["parts"], 1)
        self.assertGreater(receipts[-1]["bytes"], 0)
        self.assertFalse((self.profile / "incremental").exists())
        self.assertEqual(list(self.target.glob(filesystem.PREFIX + "*")), [])
        self.assertEqual(summary["removed"], "0")
        self.assertEqual(summary["incremental_reclaims"], "1")
        self.assertEqual(summary["pending_candidates"], "0")
        self.assert_preserved()

    def test_dry_run_measures_nonempty_parts_without_mutation(self):
        _, receipts, summary = self.collect("--dry-run", "--prune-incremental-entry", "selected")
        self.assertEqual(receipts[0]["status"], "dry-run")
        self.assertGreater(receipts[0]["bytes"], 0)
        self.assertEqual(summary["pending_candidates"], "1")
        self.assertTrue((self.profile / "incremental/cache").is_file())
        self.assert_preserved()

    def test_late_holder_restores_the_exact_staged_generation(self):
        before = (self.profile / "incremental").stat().st_ino
        count = self.root / "probe-count"
        self.probe.write_text(
            '#!/bin/sh\nn=0\nif [ -f "$TARGET_GC_TEST_COUNT" ]; then read -r n < "$TARGET_GC_TEST_COUNT"; fi\n'
            'n=$((n + 1))\nprintf "%s\\n" "$n" > "$TARGET_GC_TEST_COUNT"\n'
            'if [ "$n" -ge 3 ]; then echo busy; else echo idle; fi\n')
        env = dict(self.env, TARGET_GC_TEST_COUNT=str(count))
        _, receipts, summary = self.collect("--prune-incremental-entry", "selected", env=env)
        self.assertEqual([r["status"] for r in receipts], ["staged", "restored"])
        self.assertEqual((self.profile / "incremental").stat().st_ino, before)
        self.assertEqual(count.read_text().strip(), "3")
        self.assertEqual(summary["status"], "partial")
        self.assertEqual(summary["reclaimed_bytes"], "0")
        self.assert_preserved()

    def test_unknown_ownership_and_missing_selection_never_report_completion(self):
        self.probe.write_text("#!/bin/sh\necho unknown\n")
        _, receipts, summary = self.collect("--prune-incremental-entry", "selected")
        self.assertEqual(receipts, [])
        self.assertEqual(summary["status"], "partial")
        self.assertGreater(int(summary["pending_candidates"]), 0)
        _, receipts, summary = self.collect("--prune-incremental-entry", "absent")
        self.assertEqual(receipts, [])
        self.assertEqual(summary["blocked_named_entries"], "1")
        self.assertEqual(summary["status"], "partial")
        self.assert_preserved()

    def test_symlinked_incremental_state_cannot_reach_outside_the_entry(self):
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "authored").write_text("retain")
        shutil = filesystem.shutil
        shutil.rmtree(self.profile / "incremental")
        (self.profile / "incremental").symlink_to(outside, target_is_directory=True)
        _, receipts, summary = self.collect("--prune-incremental-entry", "selected")
        self.assertEqual(receipts[-1]["status"], "refused")
        self.assertEqual(summary["status"], "partial")
        self.assertEqual((outside / "authored").read_text(), "retain")
        self.assert_preserved()

    def test_whole_and_automatic_modes_cannot_widen_a_partial_request(self):
        for extra in (["--remove-entry", "other"], ["--host-maintenance"]):
            result = subprocess.run(["bash", str(COLLECTOR), "--prune-incremental-entry", "selected", *extra],
                                    env=self.env, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertTrue((self.profile / "incremental/cache").is_file())
            self.assert_preserved()

    @unittest.skipUnless(platform.system() == "Linux", "real /proc ownership collector")
    def test_real_open_reader_refuses_then_idle_generation_is_reclaimed(self):
        env = dict(self.env)
        env.pop("HARN_TARGET_GC_HANDLE_PROBE")
        holder_env = dict(os.environ, TARGET_GC_TEST_ARTIFACT=str(self.profile / "incremental/cache"))
        holder = subprocess.Popen([sys.executable, "-u", "-c",
                                   "import os,sys; f=open(os.environ['TARGET_GC_TEST_ARTIFACT'],'rb'); print('ready',flush=True); sys.stdin.read()"],
                                  env=holder_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), "ready")
            busy, receipts, summary = self.collect("--prune-incremental-entry", "selected", env=env)
            self.assertIn(str(holder.pid), busy.stdout)
            self.assertEqual(receipts, [])
            self.assertEqual(summary["status"], "partial")
            self.assertTrue((self.profile / "incremental/cache").is_file())
        finally:
            holder.communicate(input="release\n", timeout=15)
        _, receipts, summary = self.collect("--prune-incremental-entry", "selected", env=env)
        self.assertEqual(receipts[-1]["status"], "reclaimed")
        self.assertEqual(summary["pending_candidates"], "0")
        self.assert_preserved()

    def test_entry_replacement_invalidates_the_staged_manifest(self):
        staged = filesystem.operate("stage", str(self.targets), "selected")
        old = self.targets / "old-selected"
        self.target.rename(old)
        self.make_profile(self.target / "debug")
        (self.target / staged["stage"]).symlink_to(old / staged["stage"], target_is_directory=True)
        with self.assertRaises(OSError):
            filesystem.operate("finish", str(self.targets), "selected", stage_name=staged["stage"])
        self.assertTrue((old / staged["stage"] / "0/cache").is_file())
        self.assert_preserved()

    def test_partial_deletion_receipt_and_recovery_preserve_other_output(self):
        self.make_profile(self.target / "release")
        staged = filesystem.operate("stage", str(self.targets), "selected")
        real_remove = filesystem.shutil.rmtree
        calls = []

        def remove(path, *, dir_fd=None):
            calls.append(path)
            if len(calls) == 2:
                raise OSError("injected deletion refusal")
            return real_remove(path, dir_fd=dir_fd)

        remove.avoids_symlink_attacks = real_remove.avoids_symlink_attacks
        with patch.object(filesystem.shutil, "rmtree", new=remove):
            partial = filesystem.operate("finish", str(self.targets), "selected", stage_name=staged["stage"])
        self.assertEqual(partial["status"], "partial")
        self.assertGreater(partial["bytes"], 0)
        self.assertTrue((self.target / staged["stage"]).is_dir())
        resumed = filesystem.operate("stage", str(self.targets), "selected")
        self.assertEqual(resumed["stage"], staged["stage"])
        final = filesystem.operate("finish", str(self.targets), "selected", stage_name=resumed["stage"])
        self.assertEqual(final["status"], "reclaimed")
        self.assertGreater(final["bytes"], 0)
        self.assertFalse((self.target / staged["stage"]).exists())
        self.assert_preserved()

    def test_recovery_measures_surviving_bytes_instead_of_the_original_plan(self):
        staged = filesystem.operate("stage", str(self.targets), "selected")
        cache = self.target / staged["stage"] / "0/cache"
        original = cache.stat().st_blocks * 512 + cache.parent.stat().st_blocks * 512
        cache.write_text("remaining")
        remaining = cache.stat().st_blocks * 512 + cache.parent.stat().st_blocks * 512
        self.assertGreater(remaining, 0)
        self.assertLess(remaining, original)
        dry = filesystem.operate("stage", str(self.targets), "selected", dry_run=True)
        self.assertEqual(dry["bytes"], remaining)
        final = filesystem.operate("finish", str(self.targets), "selected", stage_name=staged["stage"])
        self.assertEqual(final["bytes"], remaining)
        self.assertEqual(final["status"], "reclaimed")
        self.assert_preserved()

    def test_unproven_profile_and_symlinked_entry_are_retained(self):
        (self.profile / ".cargo-lock").unlink()
        _, receipts, summary = self.collect("--prune-incremental-entry", "selected")
        self.assertEqual(receipts[-1]["status"], "refused")
        self.assertEqual(summary["status"], "partial")
        self.assertTrue((self.profile / "incremental/cache").is_file())
        self.target.rename(self.targets / "original")
        self.target.symlink_to(self.targets / "original", target_is_directory=True)
        _, receipts, summary = self.collect("--prune-incremental-entry", "selected")
        self.assertEqual(receipts[-1]["status"], "refused")
        self.assertEqual(summary["status"], "partial")
        self.assert_preserved()

    def test_new_generation_survives_staged_recovery(self):
        staged = filesystem.operate("stage", str(self.targets), "selected")
        current = self.profile / "incremental"
        current.mkdir()
        (current / "new").write_text("new compiler generation")
        with self.assertRaises(filesystem.Refusal):
            filesystem.operate("restore", str(self.targets), "selected", stage_name=staged["stage"])
        resumed = filesystem.operate("stage", str(self.targets), "selected")
        final = filesystem.operate("finish", str(self.targets), "selected", stage_name=resumed["stage"])
        self.assertEqual(final["status"], "reclaimed")
        self.assertEqual((current / "new").read_text(), "new compiler generation")
        self.assert_preserved()


if __name__ == "__main__":
    unittest.main()
