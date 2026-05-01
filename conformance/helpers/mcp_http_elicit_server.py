#!/usr/bin/env python3
"""Streamable HTTP MCP server that elicits during tools/call."""

import json
import queue
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


SESSION_ID = "harn-elicit-session"
STREAMS = {}
PENDING = {}
LOCK = threading.Lock()
STATE = {"tool_calls": [], "elicit_response": None}


def jsonrpc_result(msg_id, result):
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def sse_frame(message):
    return (
        "event: message\n"
        + "data: "
        + json.dumps(message, separators=(",", ":"))
        + "\n\n"
    ).encode("utf-8")


def stream_for_session(session_id):
    with LOCK:
        stream = STREAMS.get(session_id)
    return stream


def handle_rpc(payload):
    method = payload.get("method")
    msg_id = payload.get("id")
    params = payload.get("params") or {}

    if method == "initialize":
        return jsonrpc_result(
            msg_id,
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}, "elicitation": {}},
                "serverInfo": {"name": "harn-http-elicit-test", "version": "1.0.0"},
            },
        )
    if method == "notifications/initialized":
        return None
    if method == "tools/list":
        return jsonrpc_result(
            msg_id,
            {
                "tools": [
                    {
                        "name": "ask",
                        "description": "Ask the client for deployment input",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"prompt": {"type": "string"}},
                            "required": ["prompt"],
                        },
                    }
                ]
            },
        )
    if method == "tools/call":
        tool_name = params.get("name")
        arguments = params.get("arguments") or {}
        STATE["tool_calls"].append({"name": tool_name, "arguments": arguments})
        if tool_name != "ask":
            return {
                "jsonrpc": "2.0",
                "id": msg_id,
                "error": {"code": -32601, "message": f"Unknown tool: {tool_name}"},
            }

        stream = stream_for_session(SESSION_ID)
        if stream is None:
            return {
                "jsonrpc": "2.0",
                "id": msg_id,
                "error": {"code": -32000, "message": "GET stream not connected"},
            }

        elicit_id = "elicit-1"
        response_queue = queue.Queue(maxsize=1)
        with LOCK:
            PENDING[elicit_id] = response_queue
        stream.put(
            {
                "jsonrpc": "2.0",
                "id": elicit_id,
                "method": "elicitation/create",
                "params": {
                    "message": arguments.get("prompt", "Choose environment"),
                    "requestedSchema": {
                        "type": "object",
                        "properties": {
                            "env": {"type": "string", "enum": ["staging", "production"]},
                            "confirm": {"type": "boolean"},
                        },
                        "required": ["env", "confirm"],
                    },
                },
            }
        )

        try:
            response = response_queue.get(timeout=10)
        except queue.Empty:
            response = {"error": {"code": -32000, "message": "elicitation timed out"}}
        finally:
            with LOCK:
                PENDING.pop(elicit_id, None)
        STATE["elicit_response"] = response
        result = response.get("result") or {}
        content = result.get("content") or {}
        text = f"{result.get('action')}:{content.get('env')}:{content.get('confirm')}"
        return jsonrpc_result(
            msg_id,
            {"content": [{"type": "text", "text": text}], "isError": False},
        )

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
        if self.path != "/mcp":
            self.send_response(404)
            self.end_headers()
            return

        session_id = self.headers.get("mcp-session-id")
        if session_id != SESSION_ID:
            self.send_response(400)
            self.end_headers()
            return

        stream = queue.Queue()
        with LOCK:
            STREAMS[session_id] = stream
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        self.wfile.write(b"id: prime\ndata: \n\n")
        self.wfile.flush()

        try:
            while True:
                message = stream.get(timeout=30)
                self.wfile.write(sse_frame(message))
                self.wfile.flush()
        except (BrokenPipeError, ConnectionError, queue.Empty):
            pass
        finally:
            with LOCK:
                if STREAMS.get(session_id) is stream:
                    STREAMS.pop(session_id, None)

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
        if "method" not in payload and ("result" in payload or "error" in payload):
            pending_id = str(payload.get("id"))
            with LOCK:
                pending = PENDING.get(pending_id)
            if pending is not None:
                pending.put(payload)
            self.send_response(202)
            self.end_headers()
            return

        response = handle_rpc(payload)
        if response is None:
            self.send_response(202)
            self.send_header("MCP-Session-Id", SESSION_ID)
            self.end_headers()
            return
        self.send_json(
            response,
            {
                "MCP-Protocol-Version": "2025-11-25",
                "MCP-Session-Id": SESSION_ID,
            },
        )

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
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    (state_dir / "port").write_text(str(server.server_port))
    try:
        server.serve_forever()
    except SystemExit:
        pass


if __name__ == "__main__":
    main()
