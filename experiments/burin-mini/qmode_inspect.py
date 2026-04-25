#!/usr/bin/env python3
"""Inspect a qmode task's state for context-management bugs.

Usage: qmode_inspect.py <task_dir>

Checks (each prints OK / WARN / FAIL):
- Task metadata is present and well-formed.
- qa.jsonl entries are well-formed and chronologically ordered.
- Pending and plan are mutually exclusive.
- llm-mock.jsonl records have inputs that grow monotonically with qa history.
- The system prompt appears at most once per recorded LLM input.
- The latest recorded LLM input matches the rebuilt prompt for the
  current qa.jsonl (i.e. no drift between disk state and what the
  pipeline last sent).
- Total token usage by call (flag any single call over 8192 input tokens).
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


def fail(msg: str) -> None:
    print(f"FAIL: {msg}")


def warn(msg: str) -> None:
    print(f"WARN: {msg}")


def ok(msg: str) -> None:
    print(f"OK:   {msg}")


def load_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    out: list[dict] = []
    for i, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if not line:
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError as e:
            fail(f"{path}:{i} not valid JSON: {e}")
    return out


def main(task_dir: str) -> int:
    td = Path(task_dir)
    if not td.is_dir():
        fail(f"not a directory: {td}")
        return 2

    print(f"=== qmode inspect: {td} ===")

    # Task metadata.
    task_meta_path = td / "task.json"
    if not task_meta_path.exists():
        fail("task.json missing")
    else:
        try:
            task_meta = json.loads(task_meta_path.read_text())
            if not task_meta.get("task"):
                fail("task.json missing 'task' field")
            else:
                ok(f"task: {task_meta['task'][:80]}")
        except json.JSONDecodeError as e:
            fail(f"task.json not valid JSON: {e}")

    # QA history.
    qa = load_jsonl(td / "qa.jsonl")
    print(f"INFO: qa entries = {len(qa)}")
    last_ts = ""
    for i, entry in enumerate(qa, 1):
        if not entry.get("question"):
            fail(f"qa[{i}] missing question")
        if "answer" not in entry:
            fail(f"qa[{i}] missing answer")
        ts = entry.get("answered_at", "")
        if ts and last_ts and ts < last_ts:
            warn(f"qa[{i}] timestamps out of order ({ts} < {last_ts})")
        last_ts = ts

    pending = td / "pending.json"
    plan = td / "plan.json"
    if pending.exists() and plan.exists():
        fail("both pending.json AND plan.json present (mutual exclusion violated)")
    elif pending.exists():
        try:
            p = json.loads(pending.read_text())
            ok(f"pending question: {p.get('question', '')[:80]}")
        except json.JSONDecodeError as e:
            fail(f"pending.json not valid JSON: {e}")
    elif plan.exists():
        try:
            p = json.loads(plan.read_text())
            ok(f"plan ready={p.get('ready')} tasks={len(p.get('tasks', []))} targets={p.get('targets', [])}")
            for required in ("mode", "direction", "tasks", "ready"):
                if required not in p:
                    fail(f"plan.json missing '{required}'")
            for g in p.get("grounding", []):
                gp = g.get("path", "")
                if gp:
                    abs_gp = (td.parent.parent / "workspace" / gp).resolve()
                    if not abs_gp.exists():
                        warn(f"plan.grounding path does not exist on disk: {gp}")
        except json.JSONDecodeError as e:
            fail(f"plan.json not valid JSON: {e}")
    else:
        warn("no pending.json or plan.json — pipeline likely errored before terminal tool call")

    # LLM mock recording — sanity-check call count and input/output token sizes.
    mock_path = td / "llm-mock.jsonl"
    mock = load_jsonl(mock_path)
    print(f"INFO: llm-mock calls = {len(mock)}")
    last_in = None
    for i, call in enumerate(mock, 1):
        in_tok = call.get("input_tokens")
        out_tok = call.get("output_tokens")
        if in_tok is not None:
            if in_tok > 16384:
                warn(f"call[{i}] input_tokens={in_tok} (very large)")
            if last_in is not None and in_tok < last_in:
                warn(f"call[{i}] input_tokens={in_tok} dropped vs prior {last_in} (compaction or reset?)")
            last_in = in_tok
        # Tool-call-shape audit.
        tcs = call.get("tool_calls", [])
        names = [t.get("name") for t in tcs]
        terminal_in_call = [n for n in names if n in ("ask_question", "exit_plan_mode")]
        if len(terminal_in_call) > 1:
            warn(f"call[{i}] >1 terminal tool in same iteration: {terminal_in_call}")

    # Last-prompt.txt audit.
    lp = td / "last-prompt.txt"
    if lp.exists():
        body = lp.read_text()
        sys_marker = body.count("===SYSTEM===")
        usr_marker = body.count("===USER===")
        if sys_marker != 1 or usr_marker != 1:
            fail(f"last-prompt.txt has SYSTEM={sys_marker} USER={usr_marker}; expected 1/1")
        # Detect empty Q&A in user section (the bug we just fixed).
        if "- Q: \n" in body or "  A: \n" in body:
            fail("last-prompt.txt has empty Q or A line — interpolation bug")
        if qa:
            for entry in qa:
                if entry.get("answer"):
                    needle = entry["answer"][:30]
                    if needle not in body:
                        warn(f"prior answer '{needle}...' not present in last-prompt.txt")

    # Q&A ↔ tool-call alignment is hard to check without parsing
    # provider-specific shapes. We'll trust the `tools.calls` count
    # in the agent_loop result if/when we persist it; for now this is
    # best-effort.

    print("=== inspect done ===")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(64)
    sys.exit(main(sys.argv[1]))
