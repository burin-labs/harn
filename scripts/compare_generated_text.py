#!/usr/bin/env python3
"""Compare generated text files while ignoring platform line endings."""

from __future__ import annotations

import pathlib
import sys


def normalize(text: str) -> str:
    return text.replace("\r\n", "\n").replace("\r", "\n")


def main() -> int:
    if len(sys.argv) != 3:
        print(
            "usage: compare_generated_text.py <committed> <generated>",
            file=sys.stderr,
        )
        return 2

    committed = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
    generated = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8")
    return 0 if normalize(committed) == normalize(generated) else 1


if __name__ == "__main__":
    raise SystemExit(main())
