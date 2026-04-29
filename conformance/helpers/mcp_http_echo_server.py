#!/usr/bin/env python3
"""Tiny HTTP MCP server for agent_loop conformance tests."""

import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path


STATE = {"list_calls": 0, "tool_calls": []}


def jsonrpc_result(msg_id, result):
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def handle_rpc(payload):
    method = payload.get("method")
    msg_id = payload.get("id")
    params = payload.get("params") or {}

    if method == "initialize":
        return jsonrpc_result(
            msg_id,
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "harn-http-mcp-test", "version": "1.0.0"},
            },
        )
    if method == "notifications/initialized":
        return None
    if method == "tools/list":
        STATE["list_calls"] += 1
        return jsonrpc_result(
            msg_id,
            {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Echo a message back",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "message": {
                                    "type": "string",
                                    "description": "Message to echo",
                                }
                            },
                            "required": ["message"],
                        },
                    }
                ]
            },
        )
    if method == "tools/call":
        tool_name = params.get("name")
        arguments = params.get("arguments") or {}
        STATE["tool_calls"].append({"name": tool_name, "arguments": arguments})
        if tool_name == "echo":
            return jsonrpc_result(
                msg_id,
                {
                    "content": [
                        {"type": "text", "text": arguments.get("message", "")}
                    ],
                    "isError": False,
                },
            )
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "error": {"code": -32601, "message": f"Unknown tool: {tool_name}"},
        }
    return {
        "jsonrpc": "2.0",
        "id": msg_id,
        "error": {"code": -32601, "message": f"Method not found: {method}"},
    }


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/__state":
            self.send_json(STATE)
            return
        self.send_response(404)
        self.end_headers()

    def do_POST(self):
        if self.path == "/__shutdown":
            self.send_json({"ok": True})
            raise SystemExit
        if self.path != "/mcp":
            self.send_response(404)
            self.end_headers()
            return
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        response = handle_rpc(payload)
        if response is None:
            self.send_response(202)
            self.end_headers()
            return
        self.send_json(response, {"MCP-Protocol-Version": "2025-11-25"})

    def send_json(self, value, headers=None):
        body = json.dumps(value).encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        for key, header_value in (headers or {}).items():
            self.send_header(key, header_value)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format, *_args):
        return


def main():
    state_dir = Path(sys.argv[1])
    state_dir.mkdir(parents=True, exist_ok=True)
    server = HTTPServer(("127.0.0.1", 0), Handler)
    (state_dir / "port").write_text(str(server.server_port))
    try:
        server.serve_forever()
    except SystemExit:
        pass


if __name__ == "__main__":
    main()
