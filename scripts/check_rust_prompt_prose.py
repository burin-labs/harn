#!/usr/bin/env python3
"""Reject new model-facing prompt prose in Rust orchestration paths."""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


PROTECTED_PATHS = [
    Path("crates/harn-vm/src/llm/agent"),
    Path("crates/harn-vm/src/llm/tools"),
    Path("crates/harn-vm/src/orchestration/workflow.rs"),
    Path("crates/harn-vm/src/orchestration/artifacts.rs"),
    Path("crates/harn-vm/src/orchestration/compaction.rs"),
    Path("crates/harn-vm/src/llm/api/completion.rs"),
]

MODEL_FACING_MARKERS = (
    "you are",
    "you must",
    "you may",
    "do not",
    "respond",
    "assistant",
    "user-visible",
    "tool call",
    "tool-call",
    "tool result",
    "runtime_feedback",
    "prompt",
    "instruction",
    "schema",
    "completion judge",
)

ALLOWLIST: dict[str, str] = {
    # Keep this list small and review every addition. Hashes are over the
    # normalized literal body, not over file paths, so moving allowed primitive
    # constants does not churn the ratchet.
    "aa6e205895b6a1f6": "deterministic assistant-history truncation marker",
    "e6f73be5c2d864fb": "deterministic assistant-history hard-cap marker",
    "f34849951800b1c3": "runtime feedback XML envelope syntax",
    "799398738e7a23dc": "final visible-text join between assistant text and tool result",
    "f039a4fe173d20ba": "parser canonical assistant_prose tag reconstruction",
    "368fa520828a9440": "completion fallback joins user system text with rendered stdlib prompt",
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


@dataclass(frozen=True)
class Finding:
    literal: Literal
    reason: str


RAW_STRING_RE = re.compile(r'(?<![A-Za-z0-9_])r(?P<hashes>#*)"(?:.|\n)*?"(?P=hashes)', re.MULTILINE)
NORMAL_STRING_RE = re.compile(r'"(?:\\.|[^"\\\n])*"')


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
        start, end = match.span()
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
            files.extend(path for path in sorted(full.rglob("*.rs")) if "tests" not in path.parts)
        elif full.exists():
            files.append(full)
    return files


def suspicious_reason(text: str) -> str | None:
    normalized = re.sub(r"\s+", " ", text).strip()
    lower = normalized.lower()
    marker_hit = any(marker in lower for marker in MODEL_FACING_MARKERS)
    if len(normalized) >= 180 and marker_hit:
        return "long model-facing literal"
    if text.count("\n") >= 2 and marker_hit:
        return "multi-line model-facing literal"
    if len(normalized) >= 100 and ("you " in lower or "respond" in lower or "assistant" in lower):
        return "instruction-like literal"
    return None


def scan(root: Path, paths: list[Path]) -> list[Finding]:
    findings: list[Finding] = []
    for path in protected_files(root, paths):
        for literal in iter_literals(path):
            reason = suspicious_reason(literal.text)
            if reason is None:
                continue
            if literal.digest in ALLOWLIST:
                continue
            findings.append(Finding(literal, reason))
    return findings


def print_findings(findings: list[Finding]) -> None:
    for finding in findings:
        literal = finding.literal
        preview = re.sub(r"\s+", " ", literal.text).strip()
        if len(preview) > 140:
            preview = preview[:137] + "..."
        print(
            f"{literal.path}:{literal.line}: {finding.reason} "
            f"(hash {literal.digest})\n  {preview}",
            file=sys.stderr,
        )


def run_self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        bad = root / "bad.rs"
        bad.write_text(
            'fn bad() { let _ = "You are an assistant. You must respond with a '
            'careful tool call, then explain the result to the user in detail '
            'without skipping any required schema fields."; }\n',
            encoding="utf-8",
        )
        ok = root / "ok.rs"
        ok.write_text(
            'const TAG: &str = "<tool_call>";\n'
            'fn err() { let _ = "tool_dispatch: missing tool name"; }\n',
            encoding="utf-8",
        )
        bad_findings = scan(root, [Path("bad.rs")])
        ok_findings = scan(root, [Path("ok.rs")])
        if len(bad_findings) != 1 or ok_findings:
            print("self-test failed", file=sys.stderr)
            print_findings(bad_findings + ok_findings)
            return 1
    print("rust prompt prose ratchet self-test OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument("--self-test", action="store_true", help="run fixture checks")
    parser.add_argument("paths", nargs="*", help="override protected paths")
    args = parser.parse_args()

    if args.self_test:
        return run_self_test()

    root = Path(args.root).resolve()
    paths = [Path(path) for path in args.paths] if args.paths else PROTECTED_PATHS
    findings = scan(root, paths)
    if findings:
        print_findings(findings)
        print(
            "\nMove model-facing wording to stdlib .harn.prompt assets, "
            "or add a reviewed hash allowlist entry for a primitive constant.",
            file=sys.stderr,
        )
        return 1
    print("rust prompt prose ratchet OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
