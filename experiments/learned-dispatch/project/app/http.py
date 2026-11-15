"""Small HTTP host for repository-specific resource modules."""

from __future__ import annotations

import importlib
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Callable

Response = tuple[int, dict | list]
Handler = Callable[[dict], Response]


class App:
    def __init__(self) -> None:
        self.routes: dict[tuple[str, str], Handler] = {}
        for source in sorted(Path(__file__).parent.joinpath("resources").glob("*.py")):
            if not source.name.startswith("_"):
                importlib.import_module(f"app.resources.{source.stem}").mount(self)

    def route(self, method: str, path: str, handler: Handler) -> None:
        key = (method, path)
        if key in self.routes:
            raise ValueError(f"duplicate route: {method} {path}")
        self.routes[key] = handler

    def dispatch(self, method: str, path: str, payload: dict | None = None) -> Response:
        if method == "GET" and path == "/health":
            return 200, {"status": "ok"}
        handler = self.routes.get((method, path))
        if handler is None:
            return 404, {"error": "not_found"}
        if payload is not None and not isinstance(payload, dict):
            return 400, {"error": "object_required"}
        return handler({} if payload is None else payload)


def serve(port: int = 8080) -> None:
    app = App()

    class HttpHandler(BaseHTTPRequestHandler):
        def handle_request(self) -> None:
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if length < 0 or length > 65_536:
                    self.send_error(413)
                    return
                payload = json.loads(self.rfile.read(length)) if length else None
                status, result = app.dispatch(self.command, self.path, payload)
            except (ValueError, UnicodeDecodeError):
                status, result = 400, {"error": "invalid_json"}
            body = json.dumps(result).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        do_GET = handle_request
        do_POST = handle_request

    with ThreadingHTTPServer(("127.0.0.1", port), HttpHandler) as server:
        server.serve_forever()


if __name__ == "__main__":
    serve()
