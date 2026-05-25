#!/usr/bin/env python3
"""Reject new long Rust-owned prompt prose in protected orchestration paths."""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


DEFAULT_THRESHOLD = 200
DEFAULT_ALLOWLIST = Path("scripts/allowed_long_strings.txt")
MAX_ALLOWLIST_ENTRIES = 10
PROTECTED_PATHS = [
    Path("crates/harn-vm/src/llm"),
    Path("crates/harn-vm/src/orchestration/workflow.rs"),
    Path("crates/harn-vm/src/orchestration/artifacts.rs"),
    Path("crates/harn-vm/src/orchestration/compaction.rs"),
]
EXCLUDED_PROTECTED_FILES = {
    Path("crates/harn-vm/src/llm/api/transport.rs"),
    Path("crates/harn-vm/src/llm/api/options.rs"),
}


@dataclass(frozen=True)
class Literal:
    path: Path
    line: int
    text: str

    @property
    def digest(self) -> str:
        normalized = re.sub(r"\s+", " ", self.text).strip()
        return hashlib.sha256(normalized.encode("utf-8")).hexdigest()[:16]

    @property
    def location(self) -> str:
        return f"{self.path.as_posix()}:{self.line}"


@dataclass(frozen=True)
class Finding:
    literal: Literal
    length: int


RAW_STRING_RE = re.compile(r'(?<![A-Za-z0-9_])r(?P<hashes>#*)"(?:.|\n)*?"(?P=hashes)', re.MULTILINE)
NORMAL_STRING_RE = re.compile(r'"(?:\\.|[^"\\\n])*"')
ALLOWLIST_RE = re.compile(r"^(?P<location>[^#\s].*?:\d+)\s+#\s+(?P<justification>\S.*)$")


def decode_normal_literal(raw: str) -> str:
    body = raw[1:-1]
    try:
        return bytes(body, "utf-8").decode("unicode_escape")
    except UnicodeDecodeError:
        return body


def decode_raw_literal(raw: str) -> str:
    prefix_end = raw.index('"')
    suffix_start = raw.rindex('"')
    return raw[prefix_end + 1 : suffix_start]


def line_for_offset(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def iter_literals(path: Path) -> list[Literal]:
    source = path.read_text(encoding="utf-8")
    literals: list[Literal] = []
    raw_spans: list[tuple[int, int]] = []
    for match in RAW_STRING_RE.finditer(source):
        raw_spans.append(match.span())
        literals.append(
            Literal(path, line_for_offset(source, match.start()), decode_raw_literal(match.group(0)))
        )

    for match in NORMAL_STRING_RE.finditer(source):
        start, _end = match.span()
        if any(raw_start <= start < raw_end for raw_start, raw_end in raw_spans):
            continue
        literals.append(
            Literal(path, line_for_offset(source, start), decode_normal_literal(match.group(0)))
        )
    return literals


def protected_files(root: Path, paths: list[Path]) -> list[Path]:
    files: list[Path] = []
    for path in paths:
        full = root / path
        if full.is_dir():
            files.extend(
                path
                for path in sorted(full.rglob("*.rs"))
                if "tests" not in path.relative_to(root).parts
                and path.relative_to(root) not in EXCLUDED_PROTECTED_FILES
            )
        elif full.exists() and full.relative_to(root) not in EXCLUDED_PROTECTED_FILES:
            files.append(full)
    return files


def normalized_length(text: str) -> int:
    return len(re.sub(r"\s+", " ", text).strip())


def read_allowlist(root: Path, path: Path) -> set[str]:
    allowlist_path = root / path
    if not allowlist_path.exists():
        print(f"allowlist not found: {path}", file=sys.stderr)
        raise SystemExit(1)

    allowed: set[str] = set()
    for line_number, raw_line in enumerate(allowlist_path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        match = ALLOWLIST_RE.match(line)
        if match is None:
            print(
                f"{path}:{line_number}: expected '<path>:<line> # one-line justification'",
                file=sys.stderr,
            )
            raise SystemExit(1)
        allowed.add(match.group("location"))

    if len(allowed) > MAX_ALLOWLIST_ENTRIES:
        print(
            f"{path}: allowlist has {len(allowed)} entries; keep it at <= {MAX_ALLOWLIST_ENTRIES}",
            file=sys.stderr,
        )
        raise SystemExit(1)
    return allowed


def scan(root: Path, paths: list[Path], allowed: set[str], threshold: int) -> tuple[list[Finding], set[str]]:
    findings: list[Finding] = []
    observed_long_locations: set[str] = set()
    for path in protected_files(root, paths):
        for literal in iter_literals(path):
            literal = Literal(path.relative_to(root), literal.line, literal.text)
            length = normalized_length(literal.text)
            if length < threshold:
                continue
            observed_long_locations.add(literal.location)
            if literal.location in allowed:
                continue
            findings.append(Finding(literal, length))
    return findings, observed_long_locations


def print_findings(findings: list[Finding]) -> None:
    for finding in findings:
        literal = finding.literal
        preview = re.sub(r"\s+", " ", literal.text).strip()
        if len(preview) > 140:
            preview = preview[:137] + "..."
        print(
            f"{literal.location}: long Rust string literal "
            f"({finding.length} chars, hash {literal.digest})\n  {preview}",
            file=sys.stderr,
        )


def run_self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        bad = root / "bad.rs"
        bad.write_text(
            'fn bad() { let _ = "You are an assistant. You must respond with a '
            'careful tool call, then explain the result to the user in detail '
            'without skipping any required schema fields. Include enough '
            'model-facing procedural prose here to cross the protected-path '
            'threshold and prove the ratchet catches new Rust-owned prompts."; }\n',
            encoding="utf-8",
        )
        ok = root / "ok.rs"
        ok.write_text(
            'const TAG: &str = "<tool_call>";\n'
            'fn err() { let _ = "tool_dispatch: missing tool name"; }\n',
            encoding="utf-8",
        )
        bad_findings, _ = scan(root, [Path("bad.rs")], set(), DEFAULT_THRESHOLD)
        ok_findings, _ = scan(root, [Path("ok.rs")], set(), DEFAULT_THRESHOLD)
        if len(bad_findings) != 1 or ok_findings:
            print("self-test failed", file=sys.stderr)
            print_findings(bad_findings + ok_findings)
            return 1
    print("rust prompt prose ratchet self-test OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument("--allowlist", default=str(DEFAULT_ALLOWLIST), help="path:line allowlist file")
    parser.add_argument("--threshold", type=int, default=DEFAULT_THRESHOLD, help="normalized character threshold")
    parser.add_argument("--self-test", action="store_true", help="run fixture checks")
    parser.add_argument("paths", nargs="*", help="override protected paths")
    args = parser.parse_args()

    if args.self_test:
        return run_self_test()

    root = Path(args.root).resolve()
    paths = [Path(path) for path in args.paths] if args.paths else PROTECTED_PATHS
    allowed = read_allowlist(root, Path(args.allowlist))
    findings, observed_long_locations = scan(root, paths, allowed, args.threshold)
    stale_allowed = sorted(allowed - observed_long_locations)
    if stale_allowed:
        for location in stale_allowed:
            print(f"{args.allowlist}: stale allowlist entry: {location}", file=sys.stderr)
        return 1
    if findings:
        print_findings(findings)
        print(
            f"\nMove model-facing wording to stdlib .harn.prompt assets, or add "
            f"a reviewed '{findings[0].literal.location} # justification' entry "
            f"to {args.allowlist} for a primitive constant.",
            file=sys.stderr,
        )
        return 1
    print("rust prompt prose ratchet OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
