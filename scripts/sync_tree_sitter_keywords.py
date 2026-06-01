#!/usr/bin/env python3
"""Keep the tree-sitter-harn keyword list in sync with the lexer.

`crates/harn-lexer/src/token.rs` defines `pub const KEYWORDS: &[&str]`, the
authoritative set of reserved words the runtime parser recognises.
`tree-sitter-harn/grammar/keywords.js` re-declares the same set so the
editor grammar (highlighting, structural editing, the LSP fallback parser)
agrees with the runtime on what is a keyword. Nothing previously checked
that these two lists stayed equal, so a keyword added to the language could
silently never reach tree-sitter.

Usage:
  sync_tree_sitter_keywords.py            # drift check (default); exit 1 on mismatch
  sync_tree_sitter_keywords.py --write    # rewrite keywords.js from the lexer

The check is intentionally a *set* comparison: the grammar file keeps its
own hand-curated comments and ordering (keywords are grouped by feature
there, not alphabetised), so only membership must match.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TOKEN_RS = REPO_ROOT / "crates" / "harn-lexer" / "src" / "token.rs"
KEYWORDS_JS = REPO_ROOT / "tree-sitter-harn" / "grammar" / "keywords.js"

STRING_RE = re.compile(r'"([^"\\]*)"')


def lexer_keywords() -> set[str]:
    text = TOKEN_RS.read_text(encoding="utf-8")
    m = re.search(r"pub const KEYWORDS:\s*&\[&str\]\s*=\s*&\[(.*?)\];", text, re.DOTALL)
    if not m:
        raise SystemExit(
            f"error: could not find `pub const KEYWORDS` in {TOKEN_RS}"
        )
    return set(STRING_RE.findall(m.group(1)))


def grammar_keywords() -> set[str]:
    text = KEYWORDS_JS.read_text(encoding="utf-8")
    m = re.search(r"module\.exports\s*=\s*\[(.*?)\];", text, re.DOTALL)
    if not m:
        raise SystemExit(
            f"error: could not find `module.exports = [ ... ]` in {KEYWORDS_JS}"
        )
    # Strip // line comments so commented prose isn't scanned for strings.
    body = re.sub(r"//[^\n]*", "", m.group(1))
    return set(STRING_RE.findall(body))


def write_keywords_js(keywords: set[str]) -> None:
    ordered = sorted(keywords)
    lines = [
        "// @generated from crates/harn-lexer/src/token.rs (KEYWORDS).",
        "// Regenerate with `make gen-tree-sitter-keywords`; the lexer is the",
        "// source of truth. `make check-tree-sitter-keywords` guards drift.",
        "module.exports = [",
    ]
    lines += [f'  "{kw}",' for kw in ordered]
    lines.append("];")
    KEYWORDS_JS.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write",
        action="store_true",
        help="rewrite keywords.js from the lexer instead of just checking.",
    )
    args = parser.parse_args()

    lexer = lexer_keywords()
    grammar = grammar_keywords()

    if args.write:
        write_keywords_js(lexer)
        print(f"    Wrote {len(lexer)} keywords to {KEYWORDS_JS.relative_to(REPO_ROOT)}.")
        return 0

    missing = lexer - grammar  # in lexer, absent from grammar
    extra = grammar - lexer  # in grammar, not a real keyword

    if missing or extra:
        print(
            "error: tree-sitter keyword list is out of sync with the lexer "
            "(crates/harn-lexer/src/token.rs KEYWORDS):",
            file=sys.stderr,
        )
        if missing:
            print(
                f"  - missing from tree-sitter-harn/grammar/keywords.js: "
                f"{', '.join(sorted(missing))}",
                file=sys.stderr,
            )
        if extra:
            print(
                f"  - present in keywords.js but not a lexer keyword: "
                f"{', '.join(sorted(extra))}",
                file=sys.stderr,
            )
        print(
            "\nhint: run `make gen-tree-sitter-keywords` to regenerate from the "
            "lexer, or reconcile the lists by hand.",
            file=sys.stderr,
        )
        return 1

    print(f"    tree-sitter keyword list OK ({len(lexer)} keywords).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
