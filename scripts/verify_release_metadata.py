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


def is_bump_ahead(cargo_version: str, changelog_version: str) -> bool:
    """True when the top CHANGELOG entry is exactly one patch/minor/major
    step ahead of the current Cargo.toml version. This is the intermediate
    "release content committed, version bump not yet applied" state that the
    release workflow intentionally produces, and the audit must pass in that
    state so release_ship.sh can run end-to-end without a manual bump first.
    """
    cur = parse_semver(cargo_version)
    nxt = parse_semver(changelog_version)
    if nxt <= cur:
        return False
    patch_bump = (cur[0], cur[1], cur[2] + 1)
    minor_bump = (cur[0], cur[1] + 1, 0)
    major_bump = (cur[0] + 1, 0, 0)
    return nxt in (patch_bump, minor_bump, major_bump)


def main() -> int:
    version = current_version()
    versions = changelog_versions()
    if not versions:
        raise SystemExit("error: CHANGELOG.md does not contain any version headings")
    top = versions[0]
    effective_version = version
    if top != version:
        if is_bump_ahead(version, top):
            # Pre-bump state: CHANGELOG already describes the next release.
            # Verify release notes + tag state against the CHANGELOG version so
            # the rest of the audit sees the same version the upcoming bump
            # will produce.
            effective_version = top
        else:
            raise SystemExit(
                f"error: Cargo.toml version {version} does not match top changelog entry {top} "
                "(and the delta is not a single patch/minor/major bump)"
            )
    if len(versions) != len(set(versions)):
        raise SystemExit("error: CHANGELOG.md contains duplicate version headings")
    sorted_versions = sorted(versions, key=parse_semver, reverse=True)
    if versions != sorted_versions:
        raise SystemExit("error: CHANGELOG.md versions are not in descending semver order")
    verify_release_notes(effective_version)
    verify_tag_state(effective_version)
    if effective_version == version:
        print(f"verified release metadata for v{version}")
    else:
        print(
            f"verified release metadata for v{effective_version} "
            f"(Cargo.toml still at v{version}; bump pending)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
