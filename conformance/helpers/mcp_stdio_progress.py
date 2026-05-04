#!/usr/bin/env python3
"""Drive `harn mcp serve` over stdio, request a tool with a progress token,
and report the count of `notifications/progress` updates received before
the matching response (per MCP 2025-11-25 progress utility).
"""

import json
import subprocess
import sys


def main():
    if len(sys.argv) != 7:
        raise SystemExit(
            "usage: mcp_stdio_progress.py <harn_bin> <config_path> <state_dir> "
            "<tool_name> <arguments_json> <progress_token>"
        )

    harn_bin, config_path, state_dir, tool_name, arguments_json, token = sys.argv[1:7]
    arguments = json.loads(arguments_json)
    proc = subprocess.Popen(
        [
            harn_bin,
            "mcp",
            "serve",
            "--config",
            config_path,
            "--state-dir",
            state_dir,
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )

    def send(request):
        proc.stdin.write(json.dumps(request) + "\n")
        proc.stdin.flush()

    def read_until_id(target_id):
        progress_count = 0
        while True:
            line = proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP server closed stdout")
            message = json.loads(line)
            if message.get("method") == "notifications/progress" and message.get(
                "params", {}
            ).get("progressToken") == token:
                progress_count += 1
                continue
            if message.get("id") == target_id:
                return message, progress_count

    try:
        send(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "conformance", "version": "1.0.0"},
                },
            }
        )
        init, _ = read_until_id(1)
        if "error" in init:
            raise RuntimeError(f"initialize failed: {init}")

        send(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": tool_name,
                    "arguments": arguments,
                    "_meta": {"progressToken": token},
                },
            }
        )
        response, progress_count = read_until_id(2)
        if response.get("result", {}).get("isError"):
            raise RuntimeError(response["result"]["content"][0]["text"])
        result = response["result"].get("structuredContent", response["result"])
        print(json.dumps({"progress_count": progress_count, "result": result}))
    finally:
        if proc.stdin:
            proc.stdin.close()
        proc.wait(timeout=10)


if __name__ == "__main__":
    main()
