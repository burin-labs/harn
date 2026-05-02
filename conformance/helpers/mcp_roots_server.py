#!/usr/bin/env python3
"""MCP server that probes Harn's client roots support over stdio."""

import json
import sys


state = {
    "roots_capability": False,
    "roots": [],
    "list_changed_count": 0,
}


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def response(msg_id, result):
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def roots_report():
    return {
        "roots_capability": state["roots_capability"],
        "roots_count": len(state["roots"]),
        "roots": state["roots"],
        "list_changed_count": state["list_changed_count"],
    }


def handle_request(msg):
    method = msg.get("method")
    msg_id = msg.get("id")
    params = msg.get("params", {})

    if msg.get("id") == "roots-1" and "result" in msg:
        state["roots"] = msg["result"].get("roots", [])
        return

    if method == "initialize":
        roots_capability = params.get("capabilities", {}).get("roots", {})
        state["roots_capability"] = roots_capability.get("listChanged") is True
        send(
            response(
                msg_id,
                {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "roots-test-server", "version": "1.0.0"},
                },
            )
        )
        return

    if method == "notifications/initialized":
        send({"jsonrpc": "2.0", "id": "roots-1", "method": "roots/list"})
        return

    if method == "notifications/roots/list_changed":
        state["list_changed_count"] += 1
        return

    if method == "tools/list":
        send(
            response(
                msg_id,
                {
                    "tools": [
                        {
                            "name": "roots_report",
                            "description": "Return observed roots handshake state",
                            "inputSchema": {"type": "object", "properties": {}},
                        }
                    ]
                },
            )
        )
        return

    if method == "tools/call" and params.get("name") == "roots_report":
        send(
            response(
                msg_id,
                {
                    "content": [
                        {"type": "text", "text": json.dumps(roots_report(), sort_keys=True)}
                    ],
                    "isError": False,
                },
            )
        )
        return

    if msg_id is not None:
        send(
            {
                "jsonrpc": "2.0",
                "id": msg_id,
                "error": {"code": -32601, "message": f"Method not found: {method}"},
            }
        )


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        handle_request(msg)


if __name__ == "__main__":
    main()
