#!/usr/bin/env python3
import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
CHANGELOG = REPO_ROOT / "CHANGELOG.md"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render GitHub release notes from CHANGELOG.md."
    )
    parser.add_argument("--version", required=True, help="Version with or without leading v.")
    parser.add_argument("--output", help="Write notes to a file instead of stdout.")
    parser.add_argument("--repo", help="Repository in owner/name form.")
    return parser.parse_args()


def normalize_version(version: str) -> str:
    return version[1:] if version.startswith("v") else version


def load_changelog() -> str:
    if not CHANGELOG.exists():
        raise SystemExit(f"error: missing {CHANGELOG}")
    return CHANGELOG.read_text(encoding="utf-8")


def changelog_versions(changelog: str) -> list[str]:
    return re.findall(r"^## v([0-9]+\.[0-9]+\.[0-9]+)$", changelog, flags=re.MULTILINE)


def extract_section(changelog: str, version: str) -> str:
    match = re.search(
        rf"(?ms)^## v{re.escape(version)}\n(.*?)(?=^## v|\Z)",
        changelog,
    )
    if not match:
        raise SystemExit(f"error: CHANGELOG.md does not contain a section for ## v{version}")
    return match.group(1).strip()


def previous_version(versions: list[str], version: str) -> str | None:
    try:
        index = versions.index(version)
    except ValueError:
        return None
    return versions[index + 1] if index + 1 < len(versions) else None


def detect_repo(explicit_repo: str | None) -> str | None:
    if explicit_repo:
        return explicit_repo
    env_repo = os_environ("GITHUB_REPOSITORY")
    if env_repo:
        return env_repo
    remote = subprocess.run(
        ["git", "remote", "get-url", "origin"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if remote.returncode != 0:
        return None
    match = re.search(r"github\.com[:/]([^/]+/[^/.]+)(?:\.git)?$", remote.stdout.strip())
    return match.group(1) if match else None


def os_environ(name: str) -> str | None:
    value = os.environ.get(name, "")
    return value or None


def render_notes(section: str, repo: str | None, prev: str | None, version: str) -> str:
    parts = [section.strip(), "", "## Install / Upgrade", "", "```bash", "cargo install harn-cli", "```"]
    if repo and prev:
        parts.extend(
            [
                "",
                f"**Full Changelog**: https://github.com/{repo}/compare/v{prev}...v{version}",
            ]
        )
    return "\n".join(parts) + "\n"


def main() -> int:
    args = parse_args()
    version = normalize_version(args.version)
    changelog = load_changelog()
    versions = changelog_versions(changelog)
    section = extract_section(changelog, version)
    prev = previous_version(versions, version)
    repo = detect_repo(args.repo)
    output = render_notes(section, repo, prev, version)
    if args.output:
      Path(args.output).write_text(output, encoding="utf-8")
    else:
      sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
