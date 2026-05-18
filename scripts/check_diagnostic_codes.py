#!/usr/bin/env python3
"""Static checks for stable Harn diagnostic codes."""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "crates/harn-parser/src/diagnostic_codes.rs"
CATEGORIES = {
    "Typ",
    "Par",
    "Nam",
    "Cap",
    "Llm",
    "Orc",
    "Std",
    "Prm",
    "Mod",
    "Rmd",
    "Lnt",
    "Fmt",
    "Imp",
    "Own",
    "Rcv",
    "Mat",
    "Pol",
}
HELPERS = [
    "error_at_with_help",
    "error_at_with_fix",
    "error_at",
    "type_mismatch_at",
    "exhaustiveness_error_with_missing",
    "exhaustiveness_error_at",
    "warning_at_with_help",
    "warning_at",
    "lint_warning_at_with_fix",
]


def main() -> int:
    errors: list[str] = []
    code_names = check_registry(errors)
    check_struct_literals(errors, "LintDiagnostic", ROOT / "crates/harn-lint/src")
    check_struct_literals(errors, "TypeDiagnostic", ROOT / "crates/harn-parser/src")
    check_struct_literals(errors, "PreflightDiagnostic", ROOT / "crates/harn-cli/src/commands/check")
    check_typechecker_helper_calls(errors)
    check_code_constructor_calls(errors, ROOT / "crates/harn-fmt/src", "FormatError::new")
    check_unknown_code_variants(errors, code_names)
    if errors:
        print("diagnostic code check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("diagnostic codes: OK")
    return 0


def check_registry(errors: list[str]) -> set[str]:
    text = REGISTRY.read_text()
    entries = re.findall(
        r"^\s*([A-Za-z][A-Za-z0-9_]*),\s+\"([^\"]+)\",\s+([A-Za-z]+),",
        text,
        re.MULTILINE,
    )
    if len(entries) < 40:
        errors.append(f"{REGISTRY}: expected at least 40 registered codes, found {len(entries)}")
    seen_ids: set[str] = set()
    seen_variants: set[str] = set()
    seen_categories: set[str] = set()
    for variant, identifier, category in entries:
        if identifier in seen_ids:
            errors.append(f"{REGISTRY}: duplicate diagnostic identifier {identifier}")
        seen_ids.add(identifier)
        if variant in seen_variants:
            errors.append(f"{REGISTRY}: duplicate diagnostic variant {variant}")
        seen_variants.add(variant)
        if category not in CATEGORIES:
            errors.append(f"{REGISTRY}: unknown category enum variant {category}")
            continue
        match = re.fullmatch(r"HARN-([A-Z]{3})-\d{3}", identifier)
        if match is None:
            errors.append(f"{REGISTRY}: malformed diagnostic identifier {identifier}")
            continue
        category_text = match.group(1)
        if category_text != category_code(category):
            errors.append(
                f"{REGISTRY}: identifier {identifier} does not match category {category}"
            )
        seen_categories.add(category)
    missing = CATEGORIES - seen_categories
    if missing:
        errors.append(f"{REGISTRY}: missing populated categories {sorted(missing)}")
    return seen_variants


def category_code(category: str) -> str:
    return {
        "Typ": "TYP",
        "Par": "PAR",
        "Nam": "NAM",
        "Cap": "CAP",
        "Llm": "LLM",
        "Orc": "ORC",
        "Std": "STD",
        "Prm": "PRM",
        "Mod": "MOD",
        "Rmd": "RMD",
        "Lnt": "LNT",
        "Fmt": "FMT",
        "Imp": "IMP",
        "Own": "OWN",
        "Rcv": "RCV",
        "Mat": "MAT",
        "Pol": "POL",
    }[category]


def check_struct_literals(errors: list[str], name: str, root: Path) -> None:
    # Patterns that precede `Foo {` in non-literal positions (and the
    # name itself sits at `start`, so we only need to inspect what comes
    # immediately before it).
    non_literal_prefix = re.compile(
        r"(?:\bimpl(?:\s*<[^>]*>)?\s+(?:[\w:]+::)?|->\s*(?:[\w:]+::)?)$"
    )
    for path in sorted(root.rglob("*.rs")):
        text = path.read_text()
        for start, end in find_struct_literals(text, f"{name} {{"):
            prefix = text[max(0, start - 80) : start]
            # Skip non-literal occurrences:
            #   - the struct definition itself (`pub struct Foo { ... }`)
            #   - inherent / trait impl blocks (`impl Foo { ... }`)
            #   - function return-type body openings (`-> Foo {`, `-> crate::Foo {`)
            if f"pub struct {name}" in prefix:
                continue
            if non_literal_prefix.search(prefix):
                continue
            block = text[start:end]
            if not re.search(r"\bcode\s*[:,]", block):
                errors.append(f"{rel(path)}:{line(text, start)}: {name} literal missing code")


def find_struct_literals(text: str, needle: str) -> list[tuple[int, int]]:
    out: list[tuple[int, int]] = []
    start = 0
    while True:
        idx = text.find(needle, start)
        if idx < 0:
            return out
        brace = text.find("{", idx)
        depth = 0
        end = None
        for pos in range(brace, len(text)):
            char = text[pos]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    end = pos + 1
                    break
        if end is None:
            out.append((idx, len(text)))
            return out
        out.append((idx, end))
        start = end


def check_typechecker_helper_calls(errors: list[str]) -> None:
    root = ROOT / "crates/harn-parser/src/typechecker"
    for path in sorted(root.rglob("*.rs")):
        if path.name == "mod.rs":
            continue
        text = path.read_text()
        for helper in HELPERS:
            needle = f"self.{helper}("
            start = 0
            while True:
                idx = text.find(needle, start)
                if idx < 0:
                    break
                arg = first_non_ws(text, idx + len(needle))
                if not text.startswith("Code::", arg):
                    errors.append(
                        f"{rel(path)}:{line(text, idx)}: self.{helper} must pass Code::* first"
                    )
                start = idx + len(needle)


def check_code_constructor_calls(errors: list[str], root: Path, needle: str) -> None:
    for path in sorted(root.rglob("*.rs")):
        text = path.read_text()
        start = 0
        call = f"{needle}("
        while True:
            idx = text.find(call, start)
            if idx < 0:
                break
            arg = first_non_ws(text, idx + len(call))
            if not text.startswith("Code::", arg):
                errors.append(f"{rel(path)}:{line(text, idx)}: {needle} must pass Code::* first")
            start = idx + len(call)


def check_unknown_code_variants(errors: list[str], code_names: set[str]) -> None:
    for root in [
        ROOT / "crates/harn-parser/src",
        ROOT / "crates/harn-lint/src",
        ROOT / "crates/harn-fmt/src",
        ROOT / "crates/harn-cli/src/commands/check",
        ROOT / "crates/harn-lsp/src",
    ]:
        for path in sorted(root.rglob("*.rs")):
            if path == REGISTRY:
                continue
            text = path.read_text()
            # Rust convention: enum variants are UpperCamelCase, methods
            # are snake_case. Only flag the former — `Code::repair_template()`
            # and other inherent methods are intentionally not variants.
            for match in re.finditer(r"\bCode::([A-Z][A-Za-z0-9_]*)", text):
                if match.group(1) not in code_names:
                    errors.append(
                        f"{rel(path)}:{line(text, match.start())}: unknown Code::{match.group(1)}"
                    )


def first_non_ws(text: str, offset: int) -> int:
    while offset < len(text) and text[offset].isspace():
        offset += 1
    return offset


def line(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def rel(path: Path) -> str:
    return str(path.relative_to(ROOT))


if __name__ == "__main__":
    raise SystemExit(main())
