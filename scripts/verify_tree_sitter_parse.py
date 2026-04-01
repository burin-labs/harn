#!/usr/bin/env python3
from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
GRAMMAR_DIR = ROOT / "tree-sitter-harn"
LIB_PATH = GRAMMAR_DIR / "harn.dylib"
CLI = GRAMMAR_DIR / "scripts" / "tree-sitter-cli.sh"
SCAN_ROOTS = [
    ROOT / "conformance" / "tests",
    ROOT / "examples",
    ROOT / "tests" / "bridge",
]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--strict",
        action="store_true",
        help="fail on any tree-sitter parse divergence in the positive source sweep",
    )
    args = parser.parse_args()

    if not GRAMMAR_DIR.exists():
        print("warning: tree-sitter-harn not present; skipping parse sweep")
        return 0

    if not LIB_PATH.exists():
        print(f"error: missing compiled tree-sitter library at {LIB_PATH}", file=sys.stderr)
        return 1
    if not CLI.exists():
        print(f"error: missing tree-sitter CLI wrapper at {CLI}", file=sys.stderr)
        return 1

    paths: list[Path] = []
    for scan_root in SCAN_ROOTS:
        if not scan_root.exists():
            continue
        paths.extend(sorted(p for p in scan_root.rglob("*.harn") if p.is_file()))

    if not paths:
        print("error: no positive .harn files found for tree-sitter parse sweep", file=sys.stderr)
        return 1

    with tempfile.NamedTemporaryFile("w", prefix="harn-tree-sitter-paths-", suffix=".txt") as handle:
        for path in paths:
            handle.write(str(path) + "\n")
        handle.flush()

        cmd = [
            str(CLI),
            "parse",
            "--quiet",
            "--lib-path",
            str(LIB_PATH),
            "--lang-name",
            "harn",
            "--paths",
            handle.name,
        ]
        result = subprocess.run(
            cmd,
            cwd=GRAMMAR_DIR,
            text=True,
            capture_output=True,
        )

    if result.returncode != 0:
        label = "tree-sitter parse sweep failed" if args.strict else "warning: tree-sitter parse sweep found grammar drift"
        stream = sys.stderr if args.strict else sys.stdout
        print(label, file=stream)
        if result.stdout:
            print(result.stdout, file=stream, end="")
        if result.stderr:
            print(result.stderr, file=stream, end="")
        if args.strict:
            return result.returncode
        return 0

    print(f"verified tree-sitter parse coverage across {len(paths)} positive .harn files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
