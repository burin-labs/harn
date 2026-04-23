#!/usr/bin/env python3
"""Print the semver bump type that takes Cargo.toml's workspace version to
CHANGELOG.md's top-entry version.

Used by .github/workflows/bump-release.yml to derive the right
`release_ship.sh --bump <type>` flag automatically when a "Prepare
vX.Y.Z release" commit lands on main. The version comparison is
canonical via CHANGELOG (verify_release_metadata.py already enforces
that the top entry is exactly one patch/minor/major step ahead of
Cargo.toml at prepare time), so this script just figures out which
step it is.

Exits 0 with `patch`, `minor`, or `major` on stdout. Exits non-zero
on shape mismatch with a human-readable error on stderr.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CARGO_PATH = ROOT / "Cargo.toml"
CHANGELOG_PATH = ROOT / "CHANGELOG.md"


def read_cargo_version() -> str:
    text = CARGO_PATH.read_text()
    match = re.search(r'^version = "([^"]+)"', text, re.MULTILINE)
    if not match:
        raise SystemExit(f"error: could not find workspace version in {CARGO_PATH}")
    return match.group(1)


def read_changelog_top_version() -> str:
    text = CHANGELOG_PATH.read_text()
    for line in text.splitlines():
        match = re.match(r"^## v(\d+\.\d+\.\d+)\s*$", line)
        if match:
            return match.group(1)
    raise SystemExit(f"error: no `## vX.Y.Z` heading found in {CHANGELOG_PATH}")


def parse(version: str) -> tuple[int, int, int]:
    parts = version.split(".")
    if len(parts) != 3:
        raise SystemExit(f"error: not semver: {version}")
    try:
        return tuple(int(p) for p in parts)  # type: ignore[return-value]
    except ValueError as exc:
        raise SystemExit(f"error: not semver: {version}") from exc


def detect(current: str, target: str) -> str:
    cmaj, cmin, cpat = parse(current)
    tmaj, tmin, tpat = parse(target)
    if (tmaj, tmin, tpat) == (cmaj + 1, 0, 0):
        return "major"
    if (tmaj, tmin, tpat) == (cmaj, cmin + 1, 0):
        return "minor"
    if (tmaj, tmin, tpat) == (cmaj, cmin, cpat + 1):
        return "patch"
    raise SystemExit(
        f"error: {current} -> {target} is not a single patch/minor/major bump"
    )


def main() -> int:
    current = read_cargo_version()
    target = read_changelog_top_version()
    if current == target:
        # Bump already applied; nothing to do. Exit 2 so the workflow
        # can distinguish "no-op" from "real error".
        print(f"warning: Cargo.toml and CHANGELOG.md both at {current}; nothing to bump", file=sys.stderr)
        return 2
    print(detect(current, target))
    return 0


if __name__ == "__main__":
    sys.exit(main())
