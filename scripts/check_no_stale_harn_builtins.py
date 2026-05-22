#!/usr/bin/env python3
"""Reject Harn snippets embedded in Rust string literals that call ambient
builtins removed by PR #2068.

`print`, `println`, `eprint`, `eprintln`, `read_line`, and `prompt_user` were
removed as top-level Harn names — they survive only as internal `__io_*`
bridges. Any Rust string literal that synthesizes Harn source and still calls
them by their bare name will hard-error at runtime with HARN-NAM-002.

The original `bytecode_cache.rs` regression slipped through because the
nextest default/ci profiles exclude `package(harn-cli) and kind(test)` —
the integration tests that actually execute synthesized Harn only run on
the nightly e2e profile. This static check fires in seconds and catches
the whole class of mistake at PR time.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
import warnings
from dataclasses import dataclass
from pathlib import Path

# Real Rust sources contain regex literals like `\w`, `\d`, etc. The
# `unicode_escape` decoder warns on every unrecognized backslash sequence.
# We only care about printable Harn snippets; the warning is noise.
warnings.filterwarnings("ignore", category=DeprecationWarning)

REMOVED_BUILTINS = ("println", "print", "eprintln", "eprint", "read_line", "prompt_user")

# Test fixtures that intentionally embed the rejected name to exercise the
# lint diagnostic or repair path. Keep this list short and explicit.
EXCLUDED_FILES = {
    # `ambient-stdio` lint and `harn fix` planner tests embed bare `println(`
    # to exercise the rejection diagnostic and the auto-repair pipeline. The
    # rejected name is the subject under test, not a stale fixture.
    Path("crates/harn-lint/src/tests/ambient_stdio.rs"),
    Path("crates/harn-cli/src/commands/fix.rs"),
}

# Match the bare builtin name followed by `(`, but not when preceded by an
# identifier character or `.` (which would make it `harness.stdio.println(`
# or `my_println(`). Applied to string-literal *bodies* only.
CALL_RE = re.compile(
    r"(?<![A-Za-z0-9_.])(" + "|".join(REMOVED_BUILTINS) + r")\s*\("
)

# Rust string literal extractors, copied from check_rust_prompt_prose.py.
# Raw strings: r"...", r#"..."#, r##"..."##, etc. Byte strings: b"..." and br#"..."#.
RAW_STRING_RE = re.compile(
    r'(?<![A-Za-z0-9_])b?r(?P<hashes>#*)"(?:.|\n)*?"(?P=hashes)',
    re.MULTILINE,
)
NORMAL_STRING_RE = re.compile(r'b?"(?:\\.|[^"\\\n])*"')


@dataclass(frozen=True)
class Finding:
    path: Path
    line: int
    builtin: str
    preview: str


def line_for_offset(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def decode_normal_literal(raw: str) -> str:
    # Strip the leading b/r prefix and surrounding quotes.
    start = raw.index('"')
    end = raw.rindex('"')
    body = raw[start + 1 : end]
    try:
        return bytes(body, "utf-8").decode("unicode_escape")
    except UnicodeDecodeError:
        return body


def decode_raw_literal(raw: str) -> str:
    start = raw.index('"')
    end = raw.rindex('"')
    return raw[start + 1 : end]


def iter_string_literals(source: str):
    raw_spans: list[tuple[int, int]] = []
    for match in RAW_STRING_RE.finditer(source):
        raw_spans.append(match.span())
        yield match.start(), decode_raw_literal(match.group(0))
    for match in NORMAL_STRING_RE.finditer(source):
        start, _ = match.span()
        if any(rs <= start < re_ for rs, re_ in raw_spans):
            continue
        yield start, decode_normal_literal(match.group(0))


def scan_file(path: Path, relative: Path) -> list[Finding]:
    if relative in EXCLUDED_FILES:
        return []
    try:
        source = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError):
        return []
    findings: list[Finding] = []
    for offset, body in iter_string_literals(source):
        for match in CALL_RE.finditer(body):
            line = line_for_offset(source, offset)
            preview = body[max(0, match.start() - 20) : match.end() + 20]
            preview = re.sub(r"\s+", " ", preview).strip()
            findings.append(
                Finding(relative, line, match.group(1), preview)
            )
    return findings


def iter_rust_files(root: Path):
    for path in sorted(root.rglob("*.rs")):
        if "target" in path.parts:
            continue
        yield path


def print_findings(findings: list[Finding]) -> None:
    for f in findings:
        print(
            f"{f.path.as_posix()}:{f.line}: stale Harn builtin `{f.builtin}` "
            f"in Rust string literal\n  {f.preview}",
            file=sys.stderr,
        )


def run_self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        # Positive: bare println(...) in a string literal must be flagged.
        bad = root / "bad.rs"
        bad.write_text(
            'fn t() { let _ = "import \\"./lib\\"\\nprintln(answer())\\n"; }\n',
            encoding="utf-8",
        )
        # Positive: byte string with bare print(.
        bad2 = root / "bad2.rs"
        bad2.write_text(
            'fn t() { let _ = b"print(\\"x\\")\\n"; }\n', encoding="utf-8"
        )
        # Negative: harness.stdio.println(...) must not be flagged.
        good = root / "good.rs"
        good.write_text(
            'fn t() { let _ = "harness.stdio.println(\\"ok\\")"; }\n',
            encoding="utf-8",
        )
        # Negative: __io_println(...) (the internal bridge) must not be flagged.
        good2 = root / "good2.rs"
        good2.write_text(
            'fn t() { let _ = "__io_println(\\"ok\\")"; }\n', encoding="utf-8"
        )
        # Negative: Rust println! macro must not be flagged (no opening paren
        # after `println` in the string; the `!` and the `(` are outside).
        good3 = root / "good3.rs"
        good3.write_text('fn t() { println!("hi"); }\n', encoding="utf-8")

        bad_findings = scan_file(bad, Path("bad.rs"))
        bad2_findings = scan_file(bad2, Path("bad2.rs"))
        good_findings = scan_file(good, Path("good.rs"))
        good2_findings = scan_file(good2, Path("good2.rs"))
        good3_findings = scan_file(good3, Path("good3.rs"))

        problems = []
        if len(bad_findings) != 1 or bad_findings[0].builtin != "println":
            problems.append(f"bad.rs: expected 1 println finding, got {bad_findings}")
        if len(bad2_findings) != 1 or bad2_findings[0].builtin != "print":
            problems.append(f"bad2.rs: expected 1 print finding, got {bad2_findings}")
        if good_findings:
            problems.append(f"good.rs: expected no findings, got {good_findings}")
        if good2_findings:
            problems.append(f"good2.rs: expected no findings, got {good2_findings}")
        if good3_findings:
            problems.append(f"good3.rs: expected no findings, got {good3_findings}")

        if problems:
            for p in problems:
                print(p, file=sys.stderr)
            return 1
    print("stale Harn builtins ratchet self-test OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument("--self-test", action="store_true", help="run fixture checks")
    args = parser.parse_args()

    if args.self_test:
        return run_self_test()

    root = Path(args.root).resolve()
    findings: list[Finding] = []
    for crates in (root / "crates",):
        if not crates.is_dir():
            continue
        for path in iter_rust_files(crates):
            findings.extend(scan_file(path, path.relative_to(root)))

    if findings:
        print_findings(findings)
        print(
            "\nThese names were removed from the public Harn surface in PR #2068. "
            "Replace bare calls with `__io_<name>(...)` (internal bridge for top-level "
            "scripts and tests) or `harness.stdio.<name>(...)` when a Harness binding "
            "is in scope. If a fixture genuinely needs to embed the old name to "
            "exercise the rejection diagnostic, add it to EXCLUDED_FILES in this "
            "script with a one-line justification.",
            file=sys.stderr,
        )
        return 1
    print("stale Harn builtins ratchet OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
