#!/usr/bin/env python3
"""Round-trip the published protocol fixture through the generated Python bindings.

Loads ``spec/protocol-artifacts/python/harn_protocol.py`` and the fixture file
``spec/protocol-artifacts/fixtures/round_trip.json``, decodes each envelope into
the corresponding dataclass, re-serializes via ``to_wire()``, and asserts byte-for-byte
parity (after JSON normalization). The script also asserts that the binding's
``HARN_PROTOCOL_ARTIFACT_VERSION`` matches the fixture's ``artifactVersion``.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any
from collections.abc import Mapping


REPO_ROOT = Path(__file__).resolve().parent.parent
ARTIFACTS = REPO_ROOT / "spec" / "protocol-artifacts"


def load_bindings():
    # Insert the python/ artifact directory onto sys.path so Python's dataclass
    # machinery can resolve the module via sys.modules during class construction.
    # spec_from_file_location alone leaves the module unregistered and Python 3.13+
    # uses sys.modules during dataclass-field type resolution.
    sys.path.insert(0, str(ARTIFACTS / "python"))
    import harn_protocol  # type: ignore[import-not-found]

    return harn_protocol


def assert_round_trip(label: str, expected: Mapping[str, Any], actual: Mapping[str, Any]) -> None:
    expected_canonical = json.dumps(expected, sort_keys=True)
    actual_canonical = json.dumps(actual, sort_keys=True)
    if expected_canonical != actual_canonical:
        raise AssertionError(
            f"{label} round-trip mismatch:\n  expected: {expected_canonical}\n  actual:   {actual_canonical}"
        )


def main() -> int:
    hp = load_bindings()
    fixture_path = ARTIFACTS / "fixtures" / "round_trip.json"
    fixture = json.loads(fixture_path.read_text())

    if fixture["artifactVersion"] != hp.HARN_PROTOCOL_ARTIFACT_VERSION:
        raise AssertionError(
            f"version drift: bindings={hp.HARN_PROTOCOL_ARTIFACT_VERSION} fixture={fixture['artifactVersion']}"
        )
    if fixture["harnAgentEventMethod"] != hp.HARN_AGENT_EVENT_METHOD:
        raise AssertionError(
            f"agent-event method drift: bindings={hp.HARN_AGENT_EVENT_METHOD} fixture={fixture['harnAgentEventMethod']}"
        )

    envelopes = fixture["envelopes"]
    cases = [
        ("ACPRequest", envelopes["request"], hp.ACPRequest),
        ("ACPResponse", envelopes["response"], hp.ACPResponse),
        ("ACPResponse (error)", envelopes["errorResponse"], hp.ACPResponse),
        (
            "ACPSessionUpdateNotification",
            envelopes["sessionUpdateNotification"],
            hp.ACPSessionUpdateNotification,
        ),
        (
            "HarnAgentEventNotification",
            envelopes["agentEventNotification"],
            hp.HarnAgentEventNotification,
        ),
        ("A2ATask", fixture["a2aTask"], hp.A2ATask),
        ("MCPTool", fixture["mcpTool"], hp.MCPTool),
        ("MCPDiscoverResult", fixture["mcpDiscoverResult"], hp.MCPDiscoverResult),
        (
            "MCPInputRequiredResult",
            fixture["mcpInputRequiredResult"],
            hp.MCPInputRequiredResult,
        ),
        (
            "MCPUnsupportedProtocolVersionError",
            fixture["mcpUnsupportedProtocolVersionError"],
            hp.MCPUnsupportedProtocolVersionError,
        ),
        ("ToolCallReceipt", fixture["toolCallReceipt"], hp.ToolCallReceipt),
    ]
    for label, payload, cls in cases:
        instance = cls.from_wire(payload)
        assert_round_trip(label, payload, instance.to_wire())

    print("python bindings: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
