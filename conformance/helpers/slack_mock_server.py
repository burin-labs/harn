#!/usr/bin/env python3

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse


def json_response(handler, status, body, headers=None):
    encoded = json.dumps(body).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(encoded)))
    handler.send_header("Connection", "close")
    if headers:
        for name, value in headers.items():
            handler.send_header(name, value)
    handler.end_headers()
    handler.wfile.write(encoded)


class SlackMockState:
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


class SlackMockHandler(BaseHTTPRequestHandler):
    server_version = "HarnSlackMock/1.0"

    def log_message(self, format, *args):
        return

    @property
    def state(self) -> SlackMockState:
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
        content_type = self.headers.get("Content-Type", "")
        if "application/json" in content_type:
            return json.loads(body.decode("utf-8"))
        if "application/x-www-form-urlencoded" in content_type:
            parsed = parse_qs(body.decode("utf-8"))
            return {
                key: values[0] if len(values) == 1 else values
                for key, values in parsed.items()
            }
        return body.decode("utf-8", errors="replace")

    def _request_headers(self):
        return {name.lower(): value for name, value in self.headers.items()}

    def _record(self, body):
        parsed = urlparse(self.path)
        self.state.record_request(self.command, parsed.path, self._request_headers(), body)

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/users.info":
            self._record(
                {
                    key: values[0] if len(values) == 1 else values
                    for key, values in parse_qs(parsed.query).items()
                }
            )
            json_response(
                self,
                200,
                {
                    "ok": True,
                    "user": {"id": "U123ABC456", "name": "roadrunner"},
                },
            )
            return
        if parsed.path == "/__state":
            json_response(self, 200, self.state.snapshot())
            return
        json_response(self, 404, {"ok": False, "error": f"unhandled GET {parsed.path}"})

    def do_POST(self):
        parsed = urlparse(self.path)
        body = self._parsed_body()
        if parsed.path == "/__shutdown":
            json_response(self, 202, {"ok": True})
            threading.Thread(target=self.server.shutdown, daemon=True).start()
            return
        if parsed.path == "/chat.postMessage":
            self._record(body)
            json_response(
                self,
                200,
                {
                    "ok": True,
                    "channel": body["channel"],
                    "ts": "1715.000100",
                    "message": {"text": body["text"]},
                },
            )
            return
        if parsed.path == "/chat.update":
            self._record(body)
            json_response(
                self,
                200,
                {
                    "ok": True,
                    "channel": body["channel"],
                    "ts": body["ts"],
                    "text": body["text"],
                },
            )
            return
        if parsed.path == "/reactions.add":
            self._record(body)
            json_response(
                self,
                200,
                {
                    "ok": True,
                    "type": "event_callback",
                    "event": {
                        "type": "reaction_added",
                        "reaction": body["name"],
                        "item": {
                            "type": "message",
                            "channel": body["channel"],
                            "ts": body["timestamp"],
                        },
                    },
                },
            )
            return
        if parsed.path == "/views.open":
            self._record(body)
            json_response(
                self,
                200,
                {"ok": True, "view": {"id": "V123ABC456", "type": "modal"}},
            )
            return
        if parsed.path == "/auth.test":
            self._record(body)
            json_response(
                self,
                200,
                {"ok": True, "team": "Example", "user": "bot"},
            )
            return
        json_response(self, 404, {"ok": False, "error": f"unhandled POST {parsed.path}"})


def main():
    if len(sys.argv) != 2:
        print("usage: slack_mock_server.py <state-dir>", file=sys.stderr)
        return 2
    state_dir = Path(sys.argv[1]).resolve()
    state_dir.mkdir(parents=True, exist_ok=True)
    state = SlackMockState(state_dir)
    server = ThreadingHTTPServer(("127.0.0.1", 0), SlackMockHandler)
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
