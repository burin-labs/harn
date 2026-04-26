#!/usr/bin/env python3
from __future__ import annotations

import os
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SPEC_PATH = ROOT / "spec" / "HARN_SPEC.md"


def slugify(value: str) -> str:
    value = value.strip().lower()
    value = re.sub(r"[^a-z0-9]+", "-", value)
    return value.strip("-") or "section"


def extract_harn_fences(markdown: str) -> list[tuple[str, int, str]]:
    fences: list[tuple[str, int, str]] = []
    current_heading = "spec"
    in_fence = False
    fence_lang = ""
    fence_start = 0
    fence_lines: list[str] = []

    for line_no, line in enumerate(markdown.splitlines(), start=1):
        if not in_fence and line.startswith("#"):
            current_heading = re.sub(r"^#+\s*", "", line).strip() or current_heading
            continue

        # CommonMark info string is everything after ``` up to end of
        # line (no backticks). We must accept commas etc. so that
        # `harn,ignore` — and any other-language fence like `ebnf` —
        # is recognized as a fence boundary rather than treated as
        # prose, which previously flipped fence state and silently
        # dropped every ```harn block that followed.
        match = re.match(r"^```([^`]*)$", line)
        if match:
            if not in_fence:
                in_fence = True
                fence_lang = match.group(1).strip().lower()
                fence_start = line_no + 1
                fence_lines = []
            else:
                # Only bare ```harn fences feed into the checker.
                # ```harn,ignore is an intentional fragment; other
                # languages (rust, ebnf, etc.) are never Harn code.
                if fence_lang == "harn":
                    snippet = "\n".join(fence_lines).strip()
                    if snippet:
                        fences.append((current_heading, fence_start, snippet + "\n"))
                in_fence = False
                fence_lang = ""
                fence_lines = []
            continue

        if in_fence:
            fence_lines.append(line)

    return fences


def normalize_snippet(snippet: str) -> str:
    # The prose spec sometimes uses bare `...` as an omission marker inside
    # otherwise valid examples. Replace only standalone ellipses, not spread
    # syntax like `...rest`.
    return re.sub(r"(?<![\w.])\.\.\.(?![\w.])", "nil", snippet)


def should_skip_snippet(snippet: str) -> bool:
    lowered = snippet.lower()
    return "// error:" in lowered or "# error:" in lowered


def harn_check_command() -> list[str]:
    override = os.environ.get("HARN_CHECK_BIN")
    if override:
        return [override, "check"]

    metadata = subprocess.run(
        ["cargo", "metadata", "--format-version=1", "--no-deps"],
        cwd=ROOT,
        text=True,
        capture_output=True,
    )
    if metadata.returncode == 0:
        target_dir = Path(json.loads(metadata.stdout)["target_directory"])
        binary_name = "harn.exe" if sys.platform == "win32" else "harn"
        debug_binary = target_dir / "debug" / binary_name
    else:
        debug_binary = ROOT / "target" / "debug" / "harn"

    if debug_binary.exists() and os.access(debug_binary, os.X_OK):
        return [str(debug_binary), "check"]
    return ["cargo", "run", "--quiet", "--bin", "harn", "--", "check"]


def main() -> int:
    if not SPEC_PATH.exists():
        print(f"error: missing {SPEC_PATH}", file=sys.stderr)
        return 1

    fences = extract_harn_fences(SPEC_PATH.read_text())
    if not fences:
        print(f"error: no ```harn fences found in {SPEC_PATH}", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="harn-spec-verify-") as tmp:
        tmpdir = Path(tmp)
        manifest_lines = []
        for idx, (heading, line_no, snippet) in enumerate(fences, start=1):
            if should_skip_snippet(snippet):
                continue
            slug = slugify(heading)
            path = tmpdir / f"{idx:03d}_{slug}_L{line_no}.harn"
            path.write_text(normalize_snippet(snippet))
            manifest_lines.append(f"{path.name}: {heading} (spec line {line_no})")

        cmd = harn_check_command() + [str(tmpdir)]
        result = subprocess.run(
            cmd,
            cwd=ROOT,
            text=True,
            capture_output=True,
        )

        if result.returncode != 0:
            print("language spec verification failed", file=sys.stderr)
            print("", file=sys.stderr)
            print("Extracted examples:", file=sys.stderr)
            print("\n".join(manifest_lines), file=sys.stderr)
            if result.stdout:
                print("", file=sys.stderr)
                print(result.stdout, file=sys.stderr, end="")
            if result.stderr:
                print("", file=sys.stderr)
                print(result.stderr, file=sys.stderr, end="")
            return result.returncode

    print(f"verified {len(fences)} Harn code fences from spec/HARN_SPEC.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
