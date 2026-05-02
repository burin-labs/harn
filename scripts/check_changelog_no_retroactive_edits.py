#!/usr/bin/env python3
"""Fail when this commit edits a CHANGELOG.md section for a published version.

A `## vX.Y.Z` heading whose git tag `vX.Y.Z` already exists is "published"
and must not change. New entries belong under `## Unreleased` (or the next
unreleased `## vX.Y.Z` heading being prepared in a release PR).

We only flag drift that this PR introduces — pre-existing drift in main is
left alone. Compares the CHANGELOG body between a base ref (default:
`merge-base HEAD origin/main`) and HEAD, per `## vX.Y.Z` section.

Bypass for genuine fix-ups (typos, broken links, cleaning up an
already-merged retroactive entry): either set
`ALLOW_CHANGELOG_RETROACTIVE_EDIT=1` in the environment, or include a
`Allow-Retroactive-Changelog: <reason>` trailer in any commit between
the base and HEAD. The trailer makes the bypass visible in PR review.
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHANGELOG_REL = "CHANGELOG.md"

VERSION_HEADING = re.compile(r"^## v([0-9]+\.[0-9]+\.[0-9]+)$", re.MULTILINE)


def section_body(text: str, version: str) -> str | None:
    match = re.search(
        rf"(?ms)^## v{re.escape(version)}\n(.*?)(?=^## |\Z)",
        text,
    )
    return match.group(1) if match else None


def git_show(ref: str, path: str = CHANGELOG_REL) -> str | None:
    result = subprocess.run(
        ["git", "show", f"{ref}:{path}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return None
    return result.stdout


def tag_exists(tag: str) -> bool:
    result = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"],
        cwd=ROOT,
        capture_output=True,
    )
    return result.returncode == 0


def commit_messages_since(base: str) -> str:
    result = subprocess.run(
        ["git", "log", "--format=%B%x00", f"{base}..HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode == 0 and result.stdout.strip():
        return result.stdout
    # Shallow-checkout fallback: history between BASE and HEAD isn't
    # connected (depth-1 clone). Scan a generous window of recent
    # commits — covers any reasonable PR length without needing the
    # connecting history.
    fallback = subprocess.run(
        ["git", "log", "-50", "--format=%B%x00", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return fallback.stdout if fallback.returncode == 0 else ""


def has_bypass_trailer(base: str) -> bool:
    messages = commit_messages_since(base)
    return bool(re.search(r"^Allow-Retroactive-Changelog:\s*\S", messages, re.MULTILINE))


def resolve_base(base: str | None) -> str:
    if base:
        return base
    env_base = os.environ.get("CHANGELOG_GUARD_BASE")
    if env_base:
        return env_base
    # Default: merge-base with origin/main. Falls back to origin/main if a
    # merge-base can't be computed (e.g., shallow clone with no shared history).
    for candidate in ("origin/main", "main"):
        result = subprocess.run(
            ["git", "merge-base", "HEAD", candidate],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        if result.returncode == 0:
            return result.stdout.strip()
    return "HEAD~1"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base",
        help="Git ref to compare against (defaults to merge-base with origin/main).",
    )
    args = parser.parse_args()

    if os.environ.get("ALLOW_CHANGELOG_RETROACTIVE_EDIT") == "1":
        print("check_changelog_no_retroactive_edits: bypassed via env var")
        return 0

    head_text = (ROOT / CHANGELOG_REL).read_text(encoding="utf-8")
    base = resolve_base(args.base)
    base_text = git_show(base)
    if base_text is None:
        # No base to compare against — nothing to enforce.
        return 0

    if has_bypass_trailer(base):
        print(
            "check_changelog_no_retroactive_edits: bypassed via "
            "Allow-Retroactive-Changelog trailer"
        )
        return 0

    versions = VERSION_HEADING.findall(head_text)
    drift: list[str] = []
    for version in versions:
        tag = f"v{version}"
        if not tag_exists(tag):
            continue
        head_body = section_body(head_text, version)
        base_body = section_body(base_text, version)
        if head_body is None or base_body is None:
            continue
        if head_body != base_body:
            drift.append(version)

    if not drift:
        return 0

    print(
        "error: CHANGELOG.md sections for already-published versions were modified",
        f"in this change (compared against {base}):",
        file=sys.stderr,
    )
    for version in drift:
        print(f"  - v{version}", file=sys.stderr)
    print(
        "\nNew entries belong under '## Unreleased' (or the next unreleased '## vX.Y.Z'\n"
        "heading being prepared in a release PR), not under a section that has\n"
        "already shipped. Move the entry, or — if this is a deliberate fix-up\n"
        "(typo / broken link / cleaning up a previously-merged retroactive entry)\n"
        "— bypass by adding a commit trailer:\n"
        "    Allow-Retroactive-Changelog: <reason>\n"
        "or set ALLOW_CHANGELOG_RETROACTIVE_EDIT=1 in the environment.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
