#!/usr/bin/env python3
"""Reference OpenTrustGraph v0 verifier in pure Python.

This script reads an `opentrustgraph-chain/v0` envelope from stdin or a
file, recomputes every `entry_hash`, and checks the linkage against the
stored `previous_hash` / `root_hash` values. It exists as a portable
proof point that the OpenTrustGraph hash contract is interoperable with
non-Harn runtimes.

Usage:
    python3 verify_chain.py path/to/chain.json
    cat chain.json | python3 verify_chain.py

Exit codes:
    0 - chain verified
    1 - chain rejected (output explains why)
    2 - usage / IO error

Only the Python standard library is used.
"""
from __future__ import annotations

import hashlib
import json
import sys
from typing import Any, Iterator, List

CHAIN_SCHEMA = "opentrustgraph-chain/v0"
RECORD_SCHEMA = "opentrustgraph/v0"


def _canonicalize(value: Any) -> Any:
    """Recursively sort object keys at every nesting level.

    Arrays preserve element order; only object keys are sorted. This
    matches the Harn reference impl, which routes records through
    `serde_json::Value` (a BTreeMap-backed map) before hashing.
    """
    if isinstance(value, dict):
        return {key: _canonicalize(value[key]) for key in sorted(value.keys())}
    if isinstance(value, list):
        return [_canonicalize(item) for item in value]
    return value


def canonical_record_bytes(record: dict[str, Any]) -> bytes:
    """Serialize a record to the canonical JSON used for hashing.

    `entry_hash` is removed before serialization; every other key — at
    every nesting level — is emitted in lexicographic order with no
    insignificant whitespace.
    """
    without_hash = {key: value for key, value in record.items() if key != "entry_hash"}
    canonical = _canonicalize(without_hash)
    return json.dumps(canonical, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def compute_entry_hash(record: dict[str, Any]) -> str:
    digest = hashlib.sha256(canonical_record_bytes(record)).hexdigest()
    return f"sha256:{digest}"


def verify_chain(envelope: dict[str, Any]) -> List[str]:
    errors: List[str] = []

    if envelope.get("schema") != CHAIN_SCHEMA:
        errors.append(f"unsupported chain schema: {envelope.get('schema')!r}")
    chain = envelope.get("chain") or {}
    records = envelope.get("records") or []

    declared_total = chain.get("total")
    if declared_total != len(records):
        errors.append(
            f"chain.total mismatch: declared {declared_total!r}, found {len(records)}"
        )

    previous_hash: str | None = None
    for index, record in enumerate(records):
        label = f"record {index}"
        if record.get("schema") != RECORD_SCHEMA:
            errors.append(f"{label}: unsupported record schema {record.get('schema')!r}")
        expected_index = index + 1
        if record.get("chain_index") != expected_index:
            errors.append(
                f"{label}: expected chain_index {expected_index}, found {record.get('chain_index')!r}"
            )
        if record.get("previous_hash") != previous_hash:
            errors.append(
                f"{label}: previous_hash mismatch; expected {previous_hash!r}, "
                f"found {record.get('previous_hash')!r}"
            )
        recomputed = compute_entry_hash(record)
        if recomputed != record.get("entry_hash"):
            errors.append(
                f"{label}: entry_hash mismatch; recomputed {recomputed!r}, "
                f"stored {record.get('entry_hash')!r}"
            )
        approval = (record.get("metadata") or {}).get("approval") or {}
        if (
            record.get("outcome") == "success"
            and record.get("autonomy_tier") == "act_with_approval"
            and approval.get("required") is True
        ):
            approver = record.get("approver") or ""
            signatures = approval.get("signatures") or []
            if not approver.strip():
                errors.append(f"{label}: approval required but approver is empty")
            if not signatures:
                errors.append(f"{label}: approval required but signatures are empty")
        previous_hash = record.get("entry_hash")

    declared_root = chain.get("root_hash")
    if declared_root != previous_hash:
        errors.append(
            f"chain.root_hash mismatch: declared {declared_root!r}, computed {previous_hash!r}"
        )

    return errors


def _read_envelope(args: Iterator[str]) -> dict[str, Any]:
    paths = list(args)
    if not paths:
        return json.load(sys.stdin)
    if len(paths) > 1:
        print("usage: verify_chain.py [chain.json]", file=sys.stderr)
        sys.exit(2)
    with open(paths[0], "r", encoding="utf-8") as handle:
        return json.load(handle)


def main(argv: List[str]) -> int:
    try:
        envelope = _read_envelope(iter(argv[1:]))
    except (OSError, json.JSONDecodeError) as error:
        print(f"failed to read envelope: {error}", file=sys.stderr)
        return 2

    errors = verify_chain(envelope)
    if errors:
        for line in errors:
            print(line, file=sys.stderr)
        return 1

    chain = envelope.get("chain") or {}
    print(
        f"verified topic={chain.get('topic')} records={chain.get('total')} "
        f"root_hash={chain.get('root_hash')}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
