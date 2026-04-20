#!/usr/bin/env python3

import json
import subprocess
import sys


def send_request(proc, request):
    proc.stdin.write(json.dumps(request) + "\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    if not line:
        raise RuntimeError("MCP server closed stdout")
    return json.loads(line)


def main():
    if len(sys.argv) != 6:
        raise SystemExit(
            "usage: mcp_stdio_tool.py <harn_bin> <config_path> <state_dir> <tool_name> <arguments_json>"
        )

    harn_bin, config_path, state_dir, tool_name, arguments_json = sys.argv[1:6]
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

    try:
        init = send_request(
            proc,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "conformance", "version": "1.0.0"},
                },
            },
        )
        if "error" in init:
            raise RuntimeError(f"initialize failed: {init}")

        response = send_request(
            proc,
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": tool_name,
                    "arguments": arguments,
                },
            },
        )
        if response.get("result", {}).get("isError"):
            raise RuntimeError(response["result"]["content"][0]["text"])
        print(json.dumps(response["result"]["structuredContent"]))
    finally:
        if proc.stdin:
            proc.stdin.close()
        proc.wait(timeout=10)


if __name__ == "__main__":
    main()
