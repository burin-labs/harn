#!/usr/bin/env python3
from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
GRAMMAR_DIR = ROOT / "tree-sitter-harn"
# Support both macOS (.dylib) and Linux (.so) compiled libraries.
_LIB_CANDIDATES = [GRAMMAR_DIR / "harn.dylib", GRAMMAR_DIR / "harn.so"]
LIB_PATH = next((p for p in _LIB_CANDIDATES if p.exists()), _LIB_CANDIDATES[0])
CLI = GRAMMAR_DIR / "scripts" / "tree-sitter-cli.sh"
GRAMMAR_SOURCES = [
    GRAMMAR_DIR / "grammar.js",
    GRAMMAR_DIR / "grammar" / "keywords.js",
    GRAMMAR_DIR / "src" / "parser.c",
    GRAMMAR_DIR / "src" / "scanner.c",
]
SCAN_ROOTS = [
    ROOT / "conformance" / "tests",
    ROOT / "examples",
    ROOT / "tests" / "bridge",
]


def ensure_compiled_library() -> int:
    if not CLI.exists():
        print(f"error: missing tree-sitter CLI wrapper at {CLI}", file=sys.stderr)
        return 1

    source_mtime = max(
        path.stat().st_mtime for path in GRAMMAR_SOURCES if path.exists()
    )
    needs_build = not LIB_PATH.exists()
    if not needs_build:
        needs_build = LIB_PATH.stat().st_mtime < source_mtime

    if not needs_build:
        return 0

    result = subprocess.run(
        ["npm", "run", "build"],
        cwd=GRAMMAR_DIR,
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        print("error: failed to rebuild tree-sitter shared library", file=sys.stderr)
        if result.stdout:
            print(result.stdout, file=sys.stderr, end="")
        if result.stderr:
            print(result.stderr, file=sys.stderr, end="")
        return result.returncode
    return 0


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

    build_status = ensure_compiled_library()
    if build_status != 0:
        return build_status
    if not LIB_PATH.exists():
        print(f"error: missing compiled tree-sitter library at {LIB_PATH}", file=sys.stderr)
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
