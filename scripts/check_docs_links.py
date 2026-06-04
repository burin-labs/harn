#!/usr/bin/env python3
"""Check local links in docs/src Markdown files resolve to repository files."""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import unquote, urlsplit

REPO_ROOT = Path(__file__).resolve().parent.parent
DOCS_DIR = REPO_ROOT / "docs" / "src"

INLINE_LINK_RE = re.compile(
    r"!?\[[^\]\n]*(?:\][^\[\]\n]*)?\]\(([^)\s]+)(?:\s+[^)]*)?\)"
)
REFERENCE_LINK_RE = re.compile(r"^\s{0,3}\[[^\]]+\]:\s+(\S+)", re.MULTILINE)
HTML_ATTR_RE = re.compile(r"\b(?:href|src)=(['\"])(.*?)\1", re.IGNORECASE)


@dataclass(frozen=True)
class Link:
    raw_target: str
    start: int


@dataclass(frozen=True)
class BrokenLink:
    source: Path
    line: int
    target: str
    expected: Path


def strip_fenced_blocks(text: str) -> str:
    """Blank fenced code blocks while preserving line numbers."""
    lines: list[str] = []
    in_fence = False
    fence_marker = ""

    for line in text.splitlines(keepends=True):
        stripped = line.lstrip()
        marker = stripped[:3]
        is_fence = marker in {"```", "~~~"}
        if is_fence:
            if not in_fence:
                in_fence = True
                fence_marker = marker
            elif marker == fence_marker:
                in_fence = False
                fence_marker = ""
            lines.append("\n" if line.endswith("\n") else "")
            continue

        if in_fence:
            lines.append("\n" if line.endswith("\n") else "")
        else:
            lines.append(line)

    return "".join(lines)


def iter_links(text: str) -> list[Link]:
    links: list[Link] = []

    for regex in (INLINE_LINK_RE, REFERENCE_LINK_RE):
        for match in regex.finditer(text):
            links.append(Link(match.group(1), match.start(1)))

    for match in HTML_ATTR_RE.finditer(text):
        links.append(Link(match.group(2), match.start(2)))

    return sorted(links, key=lambda link: link.start)


def local_path(raw_target: str) -> str | None:
    target = raw_target.strip().strip("<>")
    if not target or target.startswith("#"):
        return None

    parts = urlsplit(target)
    if parts.scheme or parts.netloc:
        return None

    path = unquote(parts.path)
    if not path:
        return None

    return path


def candidate_paths(source: Path, link_path: str) -> list[Path]:
    if link_path.startswith("/"):
        primary = DOCS_DIR / link_path.lstrip("/")
    else:
        primary = source.parent / link_path

    candidates = [primary.resolve(strict=False)]
    if primary.suffix == ".html":
        candidates.append(primary.with_suffix(".md").resolve(strict=False))
    elif not primary.suffix:
        candidates.append(primary.with_suffix(".md").resolve(strict=False))
        candidates.append((primary / "index.md").resolve(strict=False))
    elif str(link_path).endswith("/"):
        candidates.append((primary / "index.md").resolve(strict=False))

    return candidates


def within_repo(path: Path) -> bool:
    try:
        path.relative_to(REPO_ROOT)
        return True
    except ValueError:
        return False


def display_path(path: Path) -> str:
    try:
        return str(path.relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


def main() -> int:
    checked = 0
    failures: list[BrokenLink] = []

    for source in sorted(DOCS_DIR.rglob("*.md")):
        text = strip_fenced_blocks(source.read_text(encoding="utf-8"))
        for link in iter_links(text):
            path = local_path(link.raw_target)
            if path is None:
                continue

            checked += 1
            candidates = candidate_paths(source, path)
            if any(candidate.exists() and within_repo(candidate) for candidate in candidates):
                continue

            failures.append(
                BrokenLink(
                    source=source,
                    line=text.count("\n", 0, link.start) + 1,
                    target=link.raw_target,
                    expected=candidates[0],
                )
            )

    if failures:
        print("error: dead docs internal link(s):", file=sys.stderr)
        for failure in failures:
            print(
                f"  {display_path(failure.source)}:{failure.line}: "
                f"{failure.target} -> missing {display_path(failure.expected)}",
                file=sys.stderr,
            )
        return 1

    print(f"docs links OK: {checked} local link target(s) checked")
    return 0


if __name__ == "__main__":
    sys.exit(main())
