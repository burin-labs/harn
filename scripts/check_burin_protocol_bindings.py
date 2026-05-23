#!/usr/bin/env python3
"""Compare Burin Code's vendored Harn protocol bindings with this checkout.

The Harn repo is the source of truth for the generated Swift and TypeScript
protocol artifacts Burin Code vendors. This check intentionally accepts an
explicit Burin checkout path so Harn CI can run without a sibling repository,
while release and cross-repo jobs can fail fast on drift.
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
ARTIFACTS = REPO_ROOT / "spec" / "protocol-artifacts"

TARGETS = (
    (
        ARTIFACTS / "HarnProtocol.swift",
        Path("Sources/BurinCore/ACP/HarnProtocol.generated.swift"),
    ),
    (
        ARTIFACTS / "harn-protocol.ts",
        Path("tui/src/generated/harn-protocol.ts"),
    ),
)


def normalize(text: str) -> str:
    return text.replace("\r\n", "\n").replace("\r", "\n")


def discover_burin_root() -> Path | None:
    env = os.environ.get("BURIN_CODE_ROOT")
    if env:
        return Path(env).expanduser()
    candidate = Path.home() / "projects" / "burin-code"
    if candidate.is_dir():
        return candidate
    return None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--burin-root",
        type=Path,
        default=None,
        help="Path to a burin-code checkout. Defaults to BURIN_CODE_ROOT or ~/projects/burin-code.",
    )
    parser.add_argument(
        "--required",
        action="store_true",
        help="Fail when no Burin checkout can be found instead of skipping.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    burin_root = (args.burin_root or discover_burin_root())
    if burin_root is None or not burin_root.is_dir():
        if args.required:
            print("error: Burin Code checkout not found", file=sys.stderr)
            return 1
        print("skipping Burin protocol binding drift check (no Burin Code checkout found)")
        return 0

    stale: list[str] = []
    for source, destination_relative in TARGETS:
        destination = burin_root / destination_relative
        if not destination.is_file():
            stale.append(f"{destination_relative} (missing)")
            continue
        if normalize(source.read_text()) != normalize(destination.read_text()):
            stale.append(str(destination_relative))

    if stale:
        print(
            f"error: Burin Code protocol bindings diverge from {ARTIFACTS}:",
            file=sys.stderr,
        )
        for path in stale:
            print(f"  {path}", file=sys.stderr)
        print(
            "hint: regenerate Burin's vendored bindings from this Harn checkout or the matching Harn release.",
            file=sys.stderr,
        )
        return 1

    print(f"Burin protocol bindings match {ARTIFACTS}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
