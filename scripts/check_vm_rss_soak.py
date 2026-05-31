#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "benchmarks" / "vm_memory" / "module_cycle_entry.harn"
DEFAULT_ITERATIONS = 24
DEFAULT_MAX_TAIL_GROWTH_BYTES = 32 * 1024 * 1024


def cargo_target_dir() -> Path:
    configured = os.environ.get("CARGO_TARGET_DIR")
    if configured:
        return Path(configured)
    result = subprocess.run(
        ["cargo", "metadata", "--format-version=1", "--no-deps"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=True,
    )
    metadata = json.loads(result.stdout)
    return Path(metadata["target_directory"])


def harn_binary() -> Path:
    suffix = ".exe" if sys.platform == "win32" else ""
    candidate = cargo_target_dir() / "debug" / f"harn{suffix}"
    if not candidate.is_file():
        subprocess.run(["cargo", "build", "--bin", "harn"], cwd=ROOT, check=True)
    if not candidate.is_file():
        raise SystemExit(f"error: harn binary was not built at {candidate}")
    return candidate


def env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        value = int(raw)
    except ValueError as error:
        raise SystemExit(f"error: {name} must be an integer, got {raw!r}") from error
    if value <= 0:
        raise SystemExit(f"error: {name} must be positive")
    return value


def load_rss_samples(report_path: Path) -> list[int]:
    report = json.loads(report_path.read_text())
    samples = [
        iteration.get("rss_bytes")
        for iteration in report.get("iterations", [])
        if isinstance(iteration.get("rss_bytes"), int)
    ]
    if len(samples) < 3:
        raise SystemExit("error: benchmark profile did not contain enough rss_bytes samples")
    return samples


def main() -> int:
    iterations = env_int("HARN_VM_RSS_SOAK_ITERATIONS", DEFAULT_ITERATIONS)
    max_tail_growth = env_int(
        "HARN_VM_RSS_SOAK_MAX_TAIL_GROWTH_BYTES",
        DEFAULT_MAX_TAIL_GROWTH_BYTES,
    )

    with tempfile.TemporaryDirectory(prefix="harn-vm-rss-soak-") as tmp:
        report_path = Path(tmp) / "profile.json"
        subprocess.run(
            [
                str(harn_binary()),
                "bench",
                str(FIXTURE),
                "--iterations",
                str(iterations),
                "--profile-json",
                str(report_path),
            ],
            cwd=ROOT,
            check=True,
        )
        samples = load_rss_samples(report_path)

    warmup = max(1, len(samples) // 3)
    tail = samples[warmup:]
    tail_growth = max(0, tail[-1] - min(tail))
    if tail_growth > max_tail_growth:
        raise SystemExit(
            "error: VM RSS soak grew by "
            f"{tail_growth} bytes after warmup, above {max_tail_growth} bytes"
        )
    print(
        "vm rss soak: OK "
        f"iterations={len(samples)} tail_growth_bytes={tail_growth} "
        f"limit_bytes={max_tail_growth}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
