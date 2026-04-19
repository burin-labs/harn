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
    handler.send_header("X-RateLimit-Remaining", "4999")
    if headers:
        for name, value in headers.items():
            handler.send_header(name, value)
    handler.end_headers()
    handler.wfile.write(encoded)


class GitHubMockState:
    def __init__(self, root: Path):
        self.root = root
        self.path = root / "state.json"
        self.token_requests = 0
        self.api_requests = []
        self.lock = threading.Lock()

    def snapshot(self):
        with self.lock:
            return {
                "token_requests": self.token_requests,
                "api_requests": list(self.api_requests),
            }

    def write(self):
        self.path.write_text(json.dumps(self.snapshot(), indent=2), encoding="utf-8")

    def next_token(self):
        with self.lock:
            self.token_requests += 1
            token = self.token_requests
        self.write()
        return f"token-{token}"

    def record_request(self, method: str, path: str, headers: dict, query: dict, body):
        with self.lock:
            self.api_requests.append(
                {
                    "method": method,
                    "path": path,
                    "headers": headers,
                    "query": query,
                    "body": body,
                }
            )
        self.write()


class GitHubMockHandler(BaseHTTPRequestHandler):
    server_version = "HarnGitHubMock/1.0"

    def log_message(self, format, *args):
        return

    @property
    def state(self) -> GitHubMockState:
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
        try:
            return json.loads(body.decode("utf-8"))
        except Exception:
            return body.decode("utf-8", errors="replace")

    def _request_headers(self):
        return {name.lower(): value for name, value in self.headers.items()}

    def _record(self, body):
        parsed = urlparse(self.path)
        self.state.record_request(
            self.command,
            parsed.path,
            self._request_headers(),
            {key: values[0] if len(values) == 1 else values for key, values in parse_qs(parsed.query).items()},
            body,
        )

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/__state":
            json_response(self, 200, self.state.snapshot())
            return
        if parsed.path == "/search/issues":
            self._record(None)
            json_response(
                self,
                200,
                {"total_count": 1, "items": [{"number": 7, "title": "stale"}]},
            )
            return
        if parsed.path == "/repos/octo/demo/pulls/123" and "application/vnd.github.diff" in self.headers.get("Accept", ""):
            self._record(None)
            diff = b"diff --git a/file b/file\nindex 1111111..2222222 100644\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(diff)))
            self.send_header("Connection", "close")
            self.send_header("X-RateLimit-Remaining", "4999")
            self.end_headers()
            self.wfile.write(diff)
            return
        json_response(self, 404, {"message": f"unhandled GET {parsed.path}"})

    def do_POST(self):
        parsed = urlparse(self.path)
        body = self._parsed_body()
        if parsed.path == "/app/installations/77/access_tokens":
            token = self.state.next_token()
            json_response(
                self,
                201,
                {"token": token, "expires_at": "2030-01-01T00:00:00Z"},
            )
            return
        if parsed.path == "/__shutdown":
            json_response(self, 202, {"ok": True})
            threading.Thread(target=self.server.shutdown, daemon=True).start()
            return
        if parsed.path == "/repos/octo/demo/issues/123/comments":
            self._record(body)
            json_response(self, 201, {"id": 1, "body": "commented"})
            return
        if parsed.path == "/repos/octo/demo/issues/123/labels":
            self._record(body)
            json_response(self, 200, [{"name": "bug"}, {"name": "triage"}])
            return
        if parsed.path == "/repos/octo/demo/pulls/123/requested_reviewers":
            self._record(body)
            json_response(self, 201, {"requested_reviewers": ["alice"]})
            return
        if parsed.path == "/repos/octo/demo/issues":
            self._record(body)
            json_response(self, 201, {"number": 88, "title": "created"})
            return
        json_response(self, 404, {"message": f"unhandled POST {parsed.path}"})

    def do_PUT(self):
        parsed = urlparse(self.path)
        body = self._parsed_body()
        if parsed.path == "/repos/octo/demo/pulls/123/merge":
            self._record(body)
            json_response(self, 200, {"merged": True, "message": "merged"})
            return
        json_response(self, 404, {"message": f"unhandled PUT {parsed.path}"})


def main():
    if len(sys.argv) != 2:
        print("usage: github_mock_server.py <state-dir>", file=sys.stderr)
        return 2
    state_dir = Path(sys.argv[1]).resolve()
    state_dir.mkdir(parents=True, exist_ok=True)
    state = GitHubMockState(state_dir)
    server = ThreadingHTTPServer(("127.0.0.1", 0), GitHubMockHandler)
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
