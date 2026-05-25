#!/usr/bin/env python3
"""Backfill `@effects`/`@allocation`/`@errors`/`@api_stability`/`@example`
metadata fields on every `pub fn` under `crates/harn-stdlib/src/stdlib/`.

Behavior:
  - Functions that already carry every field are skipped.
  - Functions with an existing canonical `/** ... */` block but partial
    metadata get the missing fields appended inside the block.
  - Functions without a doc block at all are skipped — adding placeholder
    prose would be lower-quality than the existing `HARN-LNT-024`/
    `HARN-LNT-049` lints that drive proper migration.

Defaults are conservative:
  - `@effects` is inferred from the function body by substring-matching
    known builtin signatures. Empty when nothing matches.
  - `@allocation` is inferred from the declared return type (heap for
    container/string returns, `stack-only` for primitives, default `heap`).
  - `@errors` defaults to `[]` since Harn rarely declares error variants
    statically.
  - `@api_stability` is `experimental` under preview-tier paths (agent,
    triggers, workflow, connectors, personas, dashboard) and `stable`
    elsewhere.
  - `@example` reuses the function name + bare parameter identifiers.

Run from the repo root:

    python3 scripts/backfill_stdlib_metadata.py
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
STDLIB = ROOT / "crates/harn-stdlib/src/stdlib"

EXPERIMENTAL_PATH_FRAGMENTS = (
    "/agent/",
    "/triggers",
    "/workflow/",
    "/connectors",
    "/personas",
    "/handoffs",
    "/triage",
    "/dashboard/",
    "/ui_resource",
    "/orchestration/",
)

EFFECT_RULES: tuple[tuple[str, str], ...] = (
    (r"\bprint\(", "stdio.write"),
    (r"\bprintln\(", "stdio.write"),
    (r"\beprintln?\(", "stdio.write"),
    (r"\bread_to_string\(", "fs.read"),
    (r"\bread_lines\(", "fs.read"),
    (r"\bread_bytes\(", "fs.read"),
    (r"\bwrite_to_file\(", "fs.write"),
    (r"\bwrite_bytes\(", "fs.write"),
    (r"\bappend_to_file\(", "fs.write"),
    (r"\bmake_dirs\(", "fs.write"),
    (r"\b__fs_[a-z_]*read", "fs.read"),
    (r"\b__fs_[a-z_]*write", "fs.write"),
    (r"\b__fs_remove", "fs.write"),
    (r"\b__fs_[a-z_]*list", "fs.read"),
    (r"\b__fs_[a-z_]*walk", "fs.read"),
    (r"\bfs::", "fs.read"),
    (r"\bllm_call\(", "llm.call"),
    (r"\bllm_complete\(", "llm.call"),
    (r"\bllm_judge\(", "llm.call"),
    (r"\b__llm_", "llm.call"),
    (r"\bhttp_get\(", "net"),
    (r"\bhttp_post\(", "net"),
    (r"\bhttp_put\(", "net"),
    (r"\bhttp_delete\(", "net"),
    (r"\bhttp_request\(", "net"),
    (r"\bweb_fetch\(", "net"),
    (r"\bweb_get\(", "net"),
    (r"\b__net_", "net"),
    (r"\bcommand_run\(", "process"),
    (r"\bshell\(", "process"),
    (r"\b__shell_", "process"),
    (r"\b__process_", "process"),
    (r"\bgit_run\b", "process"),
    (r"\bcrypto_", "crypto"),
    (r"\b__crypto_", "crypto"),
    (r"\bhash_", "crypto"),
    (r"\btimestamp\(", "time"),
    (r"\bnow\(", "time"),
    (r"\b__time_", "time"),
    (r"\bgetenv\(", "env"),
    (r"\b__env_", "env"),
    (r"\bstore_get\(", "store.read"),
    (r"\bstore_set\(", "store.write"),
    (r"\b__store_", "store.read"),
    (r"\bemit\(", "transcript.write"),
    (r"\b__transcript_", "transcript.write"),
    (r"\bhost_call\(", "host"),
    (r"\b__host_", "host"),
    (r"\bagent_call\(", "agent"),
    (r"\bagent_spawn\(", "agent"),
    (r"\b__agent_", "agent"),
    (r"\bnotion_", "net"),
    (r"\bslack_", "net"),
    (r"\blinear_", "net"),
    (r"\bgithub_", "net"),
    (r"\bmcp_", "mcp"),
    (r"\b__mcp_", "mcp"),
)


def main() -> int:
    total = 0
    backfilled = 0
    skipped_no_block = 0
    already_complete = 0
    for path in sorted(STDLIB.rglob("*.harn")):
        text = path.read_text()
        new_text, file_stats = process_file(text, path)
        total += file_stats["total"]
        backfilled += file_stats["backfilled"]
        skipped_no_block += file_stats["skipped_no_block"]
        already_complete += file_stats["already_complete"]
        if new_text != text:
            path.write_text(new_text)
    print(
        f"public fns: {total}; "
        f"already complete: {already_complete}; "
        f"backfilled: {backfilled}; "
        f"skipped (no /** */ block): {skipped_no_block}; "
        f"final coverage: {(already_complete + backfilled) / max(total, 1):.1%}"
    )
    return 0


def process_file(text: str, path: Path) -> tuple[str, dict[str, int]]:
    stats = {"total": 0, "backfilled": 0, "skipped_no_block": 0, "already_complete": 0}
    is_experimental = any(frag in str(path) for frag in EXPERIMENTAL_PATH_FRAGMENTS)

    lines = text.split("\n")
    out: list[str] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        match = re.match(r"^pub fn ([A-Za-z_][A-Za-z0-9_]*)\s*[<(]", line)
        if not match:
            out.append(line)
            i += 1
            continue

        stats["total"] += 1
        # Find the doc block immediately above the function (already emitted).
        block_end = len(out)  # exclusive: index after the last block line.
        block_start = find_doc_block_start(out)
        if block_start is None:
            stats["skipped_no_block"] += 1
            out.append(line)
            i += 1
            continue

        existing = parse_existing_metadata(out[block_start:block_end])
        all_keys = ("effects", "allocation", "errors", "api_stability", "example")
        if all(k in existing for k in all_keys):
            stats["already_complete"] += 1
            out.append(line)
            i += 1
            continue

        body, _next_i = collect_body(lines, i)
        signature, _ = collect_signature(lines, i)
        missing = [k for k in all_keys if k not in existing]
        inferred = build_inferred(
            keys=missing,
            name=match.group(1),
            signature=signature,
            body=body,
            is_experimental=is_experimental,
        )
        new_block = inject_metadata(out[block_start:block_end], inferred)
        out[block_start:block_end] = new_block
        out.append(line)
        i += 1
        stats["backfilled"] += 1
    return "\n".join(out), stats


def find_doc_block_start(out: list[str]) -> int | None:
    """Walk backward over `out` to find the start of a `/** ... */` block
    immediately above the next line being processed. Returns the index of
    the line containing `/**`, or `None` when no canonical block is
    adjacent."""
    if not out:
        return None
    j = len(out) - 1
    # Skip a single trailing blank line — most stdlib blocks sit one line
    # above the declaration, but some have a blank separator. Only one
    # blank is tolerated; anything more breaks the binding.
    if out[j].strip() == "":
        j -= 1
        if j < 0:
            return None
    last_line = out[j].rstrip()
    if not last_line.endswith("*/"):
        return None
    # Single-line `/** ... */` form.
    if last_line.lstrip().startswith("/**"):
        return j
    # Multi-line: walk up until we see `/**`.
    while j >= 0:
        s = out[j].lstrip()
        if s.startswith("/**"):
            return j
        if not (s.startswith("*") or out[j].strip() == ""):
            return None
        j -= 1
    return None


def parse_existing_metadata(block_lines: list[str]) -> dict[str, str]:
    out: dict[str, str] = {}
    for raw in block_lines:
        s = raw.strip()
        # Strip block-comment leader/trailer if present.
        if s.startswith("/**"):
            s = s[3:].lstrip()
        if s.endswith("*/"):
            s = s[:-2].rstrip()
        s = s.lstrip("*").strip()
        if not s.startswith("@"):
            continue
        rest = s[1:]
        if ":" not in rest:
            continue
        key, _, value = rest.partition(":")
        key = key.strip()
        if key in ("effects", "allocation", "errors", "api_stability", "example"):
            out[key] = value.strip()
    return out


def inject_metadata(block_lines: list[str], new_fields: dict[str, str]) -> list[str]:
    """Insert `@key: value` lines for `new_fields` inside an existing
    `/** ... */` block. Lines are inserted immediately before the closing
    `*/` so existing prose is preserved. Output preserves the leading
    indentation of the original block."""
    if not block_lines:
        return block_lines
    # Locate the line carrying the `*/` terminator. It might be the same
    # line as `/**` (single-line form) or a dedicated trailing line.
    last_idx = len(block_lines) - 1
    last_line = block_lines[last_idx]
    indent = block_lines[0][: len(block_lines[0]) - len(block_lines[0].lstrip())]
    if last_line.lstrip().startswith("/**") and last_line.rstrip().endswith("*/"):
        # Expand the single-line block into a multi-line block before
        # appending fields.
        inner = last_line.strip()[3:-2].strip()
        rebuilt = [f"{indent}/**"]
        if inner:
            for token in inner.split("\n"):
                token = token.strip()
                if token:
                    rebuilt.append(f"{indent} * {token}")
        rebuilt.append(f"{indent} */")
        block_lines = rebuilt
        last_idx = len(block_lines) - 1
    # If we expanded, last_idx now points at the new `*/`. Either way,
    # insert the new fields just before that closing line.
    insertion_pos = last_idx
    inner_indent = f"{indent} *"
    # Ensure a blank `*` separator between any prose body and the new
    # metadata when the existing block does not already end in one.
    needs_separator = (
        insertion_pos > 1
        and block_lines[insertion_pos - 1].strip() not in ("*", "")
    )
    inserted: list[str] = []
    if needs_separator:
        inserted.append(inner_indent)
    for key in ("effects", "allocation", "errors", "api_stability", "example"):
        if key not in new_fields:
            continue
        value = new_fields[key]
        inserted.append(f"{inner_indent} @{key}: {value}")
    return block_lines[:insertion_pos] + inserted + block_lines[insertion_pos:]


def collect_body(lines: list[str], start: int) -> tuple[str, int]:
    depth = 0
    seen_open = False
    body_lines: list[str] = []
    i = start
    while i < len(lines):
        line = lines[i]
        for ch in line:
            if ch == "{":
                depth += 1
                seen_open = True
            elif ch == "}":
                depth -= 1
        if seen_open:
            body_lines.append(line)
        i += 1
        if seen_open and depth == 0:
            return ("\n".join(body_lines), i)
    return ("\n".join(body_lines), i)


def collect_signature(lines: list[str], start: int) -> tuple[str, int]:
    parts: list[str] = []
    i = start
    while i < len(lines):
        parts.append(lines[i])
        if "{" in lines[i]:
            break
        i += 1
    return (" ".join(p.strip() for p in parts), i + 1)


def build_inferred(
    *,
    keys: list[str],
    name: str,
    signature: str,
    body: str,
    is_experimental: bool,
) -> dict[str, str]:
    out: dict[str, str] = {}
    if "effects" in keys:
        effects: list[str] = []
        for pattern, label in EFFECT_RULES:
            if re.search(pattern, body) and label not in effects:
                effects.append(label)
        out["effects"] = "[" + ", ".join(effects) + "]"
    if "allocation" in keys:
        out["allocation"] = infer_allocation(signature)
    if "errors" in keys:
        out["errors"] = "[]"
    if "api_stability" in keys:
        out["api_stability"] = "experimental" if is_experimental else "stable"
    if "example" in keys:
        out["example"] = derive_example(name, signature)
    return out


def infer_allocation(signature: str) -> str:
    """Infer the allocation profile from the declared return type.

    Untyped returns default to `heap` — most stdlib functions without an
    explicit return type produce strings, lists, or dicts, so `heap` is
    the safer claim for `harn graph --json` consumers.
    """
    match = re.search(r"->\s*([^{}]+?)\s*(?:\{|$)", signature)
    if not match:
        return "heap"
    ret = match.group(1).strip()
    if ret in {"nil", "void"}:
        return "stack-only"
    primitives = {"bool", "int", "float"}
    if ret in primitives:
        return "stack-only"
    return "heap"


def derive_example(name: str, signature: str) -> str:
    match = re.search(r"\((.*?)\)\s*(?:->|\{)", signature, re.DOTALL)
    if not match:
        return f"{name}()"
    raw = match.group(1).strip()
    if not raw:
        return f"{name}()"
    args: list[str] = []
    for part in split_top_level(raw):
        token = part.strip()
        if not token:
            continue
        ident = token.split(":")[0].split("=")[0].strip()
        ident = ident.lstrip("*&")
        if ident:
            args.append(ident)
    return f"{name}({', '.join(args)})"


def split_top_level(raw: str) -> list[str]:
    out: list[str] = []
    depth = 0
    start = 0
    for idx, ch in enumerate(raw):
        if ch in "([{<":
            depth += 1
        elif ch in ")]}>":
            depth -= 1
        elif ch == "," and depth == 0:
            out.append(raw[start:idx])
            start = idx + 1
    out.append(raw[start:])
    return out


if __name__ == "__main__":
    raise SystemExit(main())
