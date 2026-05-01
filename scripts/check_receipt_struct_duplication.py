#!/usr/bin/env python3
"""Reject duplicated receipt envelope structs outside harn-vm receipts."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CANONICAL_DIR = ROOT / "crates" / "harn-vm" / "src" / "receipts"
FIELD_NAMES = {"parent_run_id", "trace_id", "cost_usd"}
SKIP_PARTS = {
    ".git",
    "target",
    "node_modules",
    "portal-dist",
    "docs",
}


def iter_rust_files() -> list[Path]:
    files: list[Path] = []
    for path in ROOT.rglob("*.rs"):
        if any(part in SKIP_PARTS for part in path.relative_to(ROOT).parts):
            continue
        if path.is_relative_to(CANONICAL_DIR):
            continue
        files.append(path)
    return files


def strip_comments(text: str) -> str:
    text = re.sub(r"//.*", "", text)
    return re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)


def find_public_structs(text: str) -> list[tuple[str, int, str]]:
    structs: list[tuple[str, int, str]] = []
    for match in re.finditer(r"\bpub\s+struct\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{", text):
        name = match.group(1)
        open_brace = text.find("{", match.end() - 1)
        depth = 0
        for index in range(open_brace, len(text)):
            char = text[index]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    line = text.count("\n", 0, match.start()) + 1
                    structs.append((name, line, text[open_brace:index]))
                    break
    return structs


def main() -> int:
    violations: list[str] = []
    for path in iter_rust_files():
        text = strip_comments(path.read_text())
        for name, line, body in find_public_structs(text):
            fields = {
                field
                for field in FIELD_NAMES
                if re.search(rf"\bpub\s+{field}\s*:", body)
            }
            if fields == FIELD_NAMES:
                rel = path.relative_to(ROOT)
                violations.append(f"{rel}:{line}: pub struct {name}")

    if not violations:
        return 0

    print(
        "Receipt envelope duplication detected. Use "
        "`harn_vm::receipts::Receipt` or a projection derived from it instead.",
        file=sys.stderr,
    )
    for violation in violations:
        print(f"  {violation}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
