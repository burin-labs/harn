#!/usr/bin/env python3

import json
import os
import socket
import sys


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: http_proxy_server.py <state_dir>")

    state_dir = sys.argv[1]
    os.makedirs(state_dir, exist_ok=True)
    port_path = os.path.join(state_dir, "port")
    state_path = os.path.join(state_dir, "state.json")

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    with open(port_path, "w", encoding="utf-8") as handle:
        handle.write(str(listener.getsockname()[1]))

    conn, _ = listener.accept()
    conn.settimeout(2.0)
    buffer = b""
    while b"\r\n\r\n" not in buffer:
        chunk = conn.recv(4096)
        if not chunk:
            raise RuntimeError("request closed before headers")
        buffer += chunk
    header_bytes, body = buffer.split(b"\r\n\r\n", 1)
    lines = header_bytes.decode("utf-8", errors="replace").split("\r\n")
    method, target, _version = lines[0].split(" ", 2)
    headers = {}
    for line in lines[1:]:
        if not line:
            continue
        name, value = line.split(":", 1)
        headers[name.strip().lower()] = value.strip()
    content_length = int(headers.get("content-length", "0"))
    while len(body) < content_length:
        chunk = conn.recv(4096)
        if not chunk:
            raise RuntimeError("request closed before body")
        body += chunk

    with open(state_path, "w", encoding="utf-8") as handle:
        json.dump(
            {
                "method": method,
                "target": target,
                "headers": headers,
                "body": body.decode("utf-8", errors="replace"),
            },
            handle,
        )

    payload = b"proxied"
    response = (
        b"HTTP/1.1 200 OK\r\n"
        + f"content-length: {len(payload)}\r\n".encode("utf-8")
        + b"content-type: text/plain\r\n"
        + b"connection: close\r\n\r\n"
        + payload
    )
    conn.sendall(response)
    conn.close()
    listener.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
