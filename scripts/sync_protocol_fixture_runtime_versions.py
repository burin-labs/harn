#!/usr/bin/env python3
"""Update generated protocol fixture runtime versions during release bumps."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any


SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+$")
FIXTURE_ROOT = Path("conformance/protocols/fixtures")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--from", dest="old", required=True)
    parser.add_argument("--to", dest="new", required=True)
    return parser.parse_args()


def require_semver(label: str, value: str) -> None:
    if not SEMVER_RE.fullmatch(value):
        raise SystemExit(f"error: {label} must be X.Y.Z, got {value!r}")


def replace_value(value: Any, old: str, new: str) -> tuple[Any, int]:
    if isinstance(value, str):
        return (new, 1) if value == old else (value, 0)
    if isinstance(value, list):
        replaced = [replace_value(item, old, new) for item in value]
        return [item for item, _ in replaced], sum(count for _, count in replaced)
    if isinstance(value, dict):
        out: dict[str, Any] = {}
        count = 0
        for key, item in value.items():
            out[key], item_count = replace_value(item, old, new)
            count += item_count
        return out, count
    return value, 0


def main() -> int:
    args = parse_args()
    require_semver("--from", args.old)
    require_semver("--to", args.new)
    if args.old == args.new:
        print(f"protocol fixture runtime versions already target {args.new}")
        return 0

    if not FIXTURE_ROOT.exists():
        print(f"protocol fixture root not found: {FIXTURE_ROOT}")
        return 0

    changed_files = 0
    changed_values = 0
    for path in sorted(FIXTURE_ROOT.rglob("*.json")):
        text = path.read_text()
        data = json.loads(text)
        _updated, count = replace_value(data, args.old, args.new)
        if count == 0:
            continue
        # Keep fixture formatting stable. The parsed rewrite above tells us this
        # file has exact string-valued runtime versions; the text replacement
        # updates only those JSON string literals without reflowing arrays.
        old_literal = json.dumps(args.old)
        new_literal = json.dumps(args.new)
        updated_text = text.replace(old_literal, new_literal)
        json.loads(updated_text)
        if replace_value(json.loads(updated_text), args.old, args.new)[1] != 0:
            raise SystemExit(f"error: failed to replace all {args.old!r} values in {path}")
        path.write_text(updated_text)
        changed_files += 1
        changed_values += count

    print(
        "synced protocol fixture runtime versions: "
        f"{changed_values} value(s) across {changed_files} file(s)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
