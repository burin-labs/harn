#!/usr/bin/env bash
#
# Verify long flags in docs/src bash/sh Harn examples exist in `harn --help`.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

HARN_BIN="${HARN_BIN:-}"
if [[ -z "$HARN_BIN" ]]; then
  # Match check_docs_snippets.sh: prefer an already-built worktree binary, and
  # fall back to a quiet harn-cli build in fresh clones.
  target_dir=""
  if command -v cargo >/dev/null 2>&1; then
    target_dir="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
      | python3 -c 'import json,sys; print(json.load(sys.stdin).get("target_directory", ""))' 2>/dev/null)"
  fi
  if [[ -z "$target_dir" ]]; then
    target_dir="${CARGO_TARGET_DIR:-target}"
  fi

  if [[ -x "$target_dir/debug/harn" ]]; then
    HARN_BIN="$target_dir/debug/harn"
  else
    echo "building harn-cli (set HARN_BIN to skip)..." >&2
    cargo build -q -p harn-cli
    HARN_BIN="$target_dir/debug/harn"
  fi
fi

export HARN_BIN
python3 <<'PY'
from __future__ import annotations

import os
import re
import shlex
import subprocess
import sys
from pathlib import Path

HARN_BIN = os.environ["HARN_BIN"]
DOCS_DIR = Path("docs/src")
ALLOW_STALE = "harn-doc-cli: allow-stale"
COMMAND_SEPARATORS = {"&&", "||", ";", "|"}

help_cache: dict[tuple[str, ...], str] = {}
command_cache: dict[tuple[str, ...], set[str]] = {}
flag_cache: dict[tuple[str, ...], tuple[set[str], dict[str, bool]]] = {}


def help_text(path: tuple[str, ...]) -> str:
    if path not in help_cache:
        proc = subprocess.run(
            [HARN_BIN, *path, "--help"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
        help_cache[path] = proc.stdout if proc.returncode == 0 else ""
    return help_cache[path]


def command_names(path: tuple[str, ...]) -> set[str]:
    if path in command_cache:
        return command_cache[path]

    commands: set[str] = set()
    in_commands = False
    for line in help_text(path).splitlines():
        if line.strip() == "Commands:":
            in_commands = True
            continue
        if in_commands and not line.strip():
            break
        if in_commands:
            match = re.match(r"\s{2}([A-Za-z0-9_-]+)\b", line)
            if match:
                commands.add(match.group(1))
    command_cache[path] = commands
    return commands


def help_flags(path: tuple[str, ...]) -> tuple[set[str], dict[str, bool]]:
    if path in flag_cache:
        return flag_cache[path]

    flags: set[str] = set()
    takes_value: dict[str, bool] = {}
    for line in help_text(path).splitlines():
        for match in re.finditer(r"(?<![\w-])--([A-Za-z0-9][A-Za-z0-9-]*)\b", line):
            flag = f"--{match.group(1)}"
            flags.add(flag)
            after = line[match.end() :]
            takes_value[flag] = bool(re.match(r"\s+(?:<|\[<)", after))
    flag_cache[path] = (flags, takes_value)
    return flags, takes_value


def accepted_flags(path: tuple[str, ...]) -> set[str]:
    accepted: set[str] = set()
    for depth in range(len(path) + 1):
        flags, _ = help_flags(path[:depth])
        accepted.update(flags)
    return accepted


def logical_lines(block_lines: list[str], first_line: int):
    buffer: list[str] = []
    start_line = first_line
    allow_stale = False

    for offset, line in enumerate(block_lines):
        line_number = first_line + offset
        raw = line.rstrip("\n")
        if not buffer:
            start_line = line_number
            allow_stale = False
        if ALLOW_STALE in raw:
            allow_stale = True

        continued = raw.rstrip().endswith("\\")
        if continued:
            raw = raw.rstrip()[:-1]
        buffer.append(raw)
        if not continued:
            yield start_line, " ".join(buffer), allow_stale
            buffer = []

    if buffer:
        yield start_line, " ".join(buffer), allow_stale


def shell_tokens(command: str) -> list[str] | None:
    # shlex does not split shell separators unless they are whitespace-delimited.
    command = re.sub(r"(\|\||&&|[;|])", r" \1 ", command)
    try:
        return shlex.split(command, comments=True, posix=True)
    except ValueError:
        return None


def command_segments(tokens: list[str]):
    segment: list[str] = []
    for token in tokens:
        if token in COMMAND_SEPARATORS:
            if segment:
                yield segment
                segment = []
        else:
            segment.append(token)
    if segment:
        yield segment


def is_assignment(token: str) -> bool:
    return re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", token) is not None


def find_harn_segment(segment: list[str]) -> list[str] | None:
    index = 0
    while index < len(segment) and segment[index] in {"$", ">"}:
        index += 1
    if index < len(segment) and segment[index] == "env":
        index += 1
    while index < len(segment) and is_assignment(segment[index]):
        index += 1

    if index < len(segment) and segment[index] == "harn":
        return segment[index + 1 :]
    return None


def analyze_harn_args(args: list[str]) -> tuple[tuple[str, ...], list[str]]:
    path: list[str] = []
    missing: list[str] = []
    index = 0

    while index < len(args):
        token = args[index]
        current_path = tuple(path)

        if token == "--":
            break
        if token.startswith("--"):
            flag = token.split("=", 1)[0]
            if flag not in accepted_flags(current_path):
                missing.append(flag)

            _, takes_value = help_flags(current_path)
            if (
                "=" not in token
                and index + 1 < len(args)
                and not args[index + 1].startswith("-")
                and (takes_value.get(flag, False) or flag not in accepted_flags(current_path))
            ):
                index += 2
            else:
                index += 1
            continue
        if token.startswith("-"):
            index += 1
            continue
        if token in command_names(current_path):
            path.append(token)
        index += 1

    return tuple(path), missing


def iter_bash_blocks(path: Path):
    lines = path.read_text().splitlines()
    in_block = False
    block_start = 0
    block_lines: list[str] = []

    for line_number, line in enumerate(lines, 1):
        if not in_block:
            if re.match(r"^```(?:bash|sh)$", line):
                in_block = True
                block_start = line_number + 1
                block_lines = []
            continue

        if line.startswith("```"):
            yield block_start, block_lines
            in_block = False
            continue

        block_lines.append(line)


failures: list[tuple[Path, int, str, tuple[str, ...], list[str]]] = []
checked = 0
skipped = 0
parse_errors: list[tuple[Path, int, str]] = []

for md_file in sorted(DOCS_DIR.rglob("*.md")):
    for first_line, block_lines in iter_bash_blocks(md_file):
        for line_number, command, allow_stale in logical_lines(block_lines, first_line):
            if allow_stale:
                skipped += 1
                continue

            tokens = shell_tokens(command)
            if tokens is None:
                if "harn" in command:
                    parse_errors.append((md_file, line_number, command))
                continue

            for segment in command_segments(tokens):
                args = find_harn_segment(segment)
                if args is None:
                    continue
                path, missing = analyze_harn_args(args)
                if any(token.startswith("--") for token in args):
                    checked += 1
                if missing:
                    failures.append((md_file, line_number, " ".join(segment), path, missing))

for md_file, line_number, command in parse_errors:
    print(f"FAIL: {md_file}:{line_number}")
    print("      could not parse shell command containing harn")
    print(f"      command: {command}")

for md_file, line_number, command, path, missing in failures:
    help_target = "harn " + " ".join(path) if path else "harn"
    print(f"FAIL: {md_file}:{line_number}")
    print(f"      command: {command}")
    print(f"      checked: {help_target} --help")
    print(f"      missing long flag(s): {', '.join(missing)}")

total_failures = len(parse_errors) + len(failures)
print()
print(
    f"docs CLI flags: {checked} flag-bearing harn invocation(s) checked, "
    f"{skipped} skipped, {total_failures} failed"
)
if total_failures:
    print()
    print(f"hint: fix the docs or add inline `# {ALLOW_STALE}` for intentional stale examples.")
    sys.exit(1)
PY
