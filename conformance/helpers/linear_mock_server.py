#!/usr/bin/env python3

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse


def json_response(handler, status, body, headers=None):
    encoded = json.dumps(body).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(encoded)))
    handler.send_header("Connection", "close")
    handler.send_header("X-Complexity", "12")
    handler.send_header("X-RateLimit-Complexity-Remaining", "249988")
    if headers:
        for name, value in headers.items():
            handler.send_header(name, value)
    handler.end_headers()
    handler.wfile.write(encoded)


class LinearMockState:
    def __init__(self, root: Path):
        self.root = root
        self.path = root / "state.json"
        self.requests = []
        self.lock = threading.Lock()

    def snapshot(self):
        with self.lock:
            return {"requests": list(self.requests)}

    def write(self):
        self.path.write_text(json.dumps(self.snapshot(), indent=2), encoding="utf-8")

    def record_request(self, method: str, path: str, headers: dict, body):
        with self.lock:
            self.requests.append(
                {
                    "method": method,
                    "path": path,
                    "headers": headers,
                    "body": body,
                }
            )
        self.write()


class LinearMockHandler(BaseHTTPRequestHandler):
    server_version = "HarnLinearMock/1.0"

    def log_message(self, format, *args):
        return

    @property
    def state(self) -> LinearMockState:
        return self.server.state

    def _read_body(self):
        length = int(self.headers.get("Content-Length", "0") or "0")
        if length <= 0:
            return b""
        return self.rfile.read(length)

    def _parsed_body(self):
        body = self._read_body()
        if not body:
            return None
        return json.loads(body.decode("utf-8"))

    def _request_headers(self):
        return {name.lower(): value for name, value in self.headers.items()}

    def _record(self, body):
        parsed = urlparse(self.path)
        self.state.record_request(self.command, parsed.path, self._request_headers(), body)

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/__state":
            json_response(self, 200, self.state.snapshot())
            return
        json_response(self, 404, {"errors": [{"message": f"unhandled GET {parsed.path}"}]})

    def do_POST(self):
        parsed = urlparse(self.path)
        body = self._parsed_body()
        if parsed.path == "/__shutdown":
            json_response(self, 202, {"ok": True})
            threading.Thread(target=self.server.shutdown, daemon=True).start()
            return
        if parsed.path != "/graphql":
            json_response(self, 404, {"errors": [{"message": f"unhandled POST {parsed.path}"}]})
            return

        self._record(body)
        query = body.get("query", "")
        if "issueUpdate" in query:
            payload = {
                "data": {
                    "issueUpdate": {
                        "success": True,
                        "issue": {"id": "ISS-1", "identifier": "ENG-1", "title": "Updated title"},
                    }
                }
            }
        elif "commentCreate" in query:
            payload = {
                "data": {
                    "commentCreate": {
                        "success": True,
                        "comment": {"id": "COM-1", "body": "Looks good"},
                    }
                }
            }
        elif "searchIssues" in query:
            payload = {
                "data": {
                    "searchIssues": {
                        "nodes": [{"id": "ISS-1", "identifier": "ENG-1", "title": "connector"}]
                    }
                }
            }
        elif "viewer" in query:
            payload = {"data": {"viewer": {"id": "user-1"}}}
        else:
            payload = {
                "data": {
                    "issues": {
                        "nodes": [{"id": "ISS-1", "identifier": "ENG-1", "title": "Connector issue"}],
                        "pageInfo": {"hasNextPage": False, "endCursor": None},
                    }
                }
            }
        json_response(self, 200, payload)


def main():
    if len(sys.argv) != 2:
        print("usage: linear_mock_server.py <state-dir>", file=sys.stderr)
        return 2
    state_dir = Path(sys.argv[1]).resolve()
    state_dir.mkdir(parents=True, exist_ok=True)
    state = LinearMockState(state_dir)
    server = ThreadingHTTPServer(("127.0.0.1", 0), LinearMockHandler)
    server.state = state
    port = server.server_address[1]
    (state_dir / "port").write_text(str(port), encoding="utf-8")
    state.write()
    try:
        server.serve_forever()
    finally:
        state.write()


if __name__ == "__main__":
    raise SystemExit(main())
