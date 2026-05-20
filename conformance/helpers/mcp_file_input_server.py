#!/usr/bin/env python3
"""MCP server fixture for experimental SEP-2356 file inputs."""
import base64
import json
import sys
from urllib.parse import unquote_to_bytes


def decode_data_uri(value):
    if not isinstance(value, str) or not value.startswith("data:"):
        raise ValueError("upload must be a data URI")
    header, payload = value[5:].split(",", 1)
    parts = header.split(";")
    media_type = parts[0] or "text/plain"
    if any(part.lower() == "base64" for part in parts[1:]):
        data = base64.b64decode(payload)
    else:
        data = unquote_to_bytes(payload)
    return media_type, data


def handle_request(msg):
    method = msg.get("method")
    msg_id = msg.get("id")
    params = msg.get("params", {})

    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "test-file-input-server", "version": "1.0.0"},
            },
        }

    if method == "notifications/initialized":
        return None

    if method == "tools/list":
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "tools": [
                    {
                        "name": "inspect_upload",
                        "description": "Inspect an inline MCP file input.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "upload": {
                                    "type": "string",
                                    "format": "uri",
                                    "x-mcp-file": {
                                        "accept": ["text/*"],
                                        "maxSize": 64,
                                    },
                                }
                            },
                            "required": ["upload"],
                        },
                    }
                ]
            },
        }

    if method == "tools/call" and params.get("name") == "inspect_upload":
        try:
            media_type, data = decode_data_uri(params.get("arguments", {}).get("upload"))
            text = data.decode("utf-8")
            body = f"{media_type}:{len(data)}:{text}"
            is_error = False
        except Exception as exc:
            body = str(exc)
            is_error = True
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "content": [{"type": "text", "text": body}],
                "isError": is_error,
            },
        }

    if msg_id is not None:
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "error": {"code": -32601, "message": f"Method not found: {method}"},
        }
    return None


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        response = handle_request(msg)
        if response is not None:
            sys.stdout.write(json.dumps(response) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
