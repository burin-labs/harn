#!/usr/bin/env python3
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CARGO = ROOT / "Cargo.toml"
CHANGELOG = ROOT / "CHANGELOG.md"


def current_version() -> str:
    text = CARGO.read_text()
    match = re.search(r'^version = "([^"]+)"', text, re.M)
    if not match:
        raise SystemExit("error: missing workspace version in Cargo.toml")
    return match.group(1)


def changelog_versions() -> list[str]:
    text = CHANGELOG.read_text()
    return re.findall(r"^## v([0-9]+\.[0-9]+\.[0-9]+)$", text, re.M)


def parse_semver(version: str) -> tuple[int, int, int]:
    major, minor, patch = version.split(".")
    return int(major), int(minor), int(patch)


def verify_release_notes(version: str) -> None:
    result = subprocess.run(
        [sys.executable, "scripts/render_release_notes.py", "--version", version],
        cwd=ROOT,
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        sys.stderr.write("error: failed to render release notes for current version\n")
        sys.stderr.write(result.stderr or result.stdout)
        raise SystemExit(result.returncode)


def verify_tag_state(version: str) -> None:
    tag = f"v{version}"
    tag_lookup = subprocess.run(
        ["git", "rev-parse", "-q", "--verify", f"refs/tags/{tag}"],
        cwd=ROOT,
        text=True,
        capture_output=True,
    )
    if tag_lookup.returncode != 0:
        return
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()
    tag_commit = subprocess.run(
        ["git", "rev-list", "-n", "1", tag],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()
    if head != tag_commit:
        raise SystemExit(
            f"error: current version {version} is already tagged at {tag_commit}, "
            f"but HEAD is {head}; bump the version or move off the released tag state"
        )


def main() -> int:
    version = current_version()
    versions = changelog_versions()
    if not versions:
        raise SystemExit("error: CHANGELOG.md does not contain any version headings")
    if versions[0] != version:
        raise SystemExit(
            f"error: Cargo.toml version {version} does not match top changelog entry {versions[0]}"
        )
    if len(versions) != len(set(versions)):
        raise SystemExit("error: CHANGELOG.md contains duplicate version headings")
    sorted_versions = sorted(versions, key=parse_semver, reverse=True)
    if versions != sorted_versions:
        raise SystemExit("error: CHANGELOG.md versions are not in descending semver order")
    verify_release_notes(version)
    verify_tag_state(version)
    print(f"verified release metadata for v{version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
