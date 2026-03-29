#!/usr/bin/env python3
"""Mock host for testing harn --bridge mode.

This script acts as the host process that the harn VM communicates with
over stdin/stdout JSON-RPC. It responds to llm_call, tool_execute, and
host_call requests with predictable responses for testing.

Usage: echo '<pipeline output>' | python3 bridge_mock_host.py <harn_binary> <pipeline.harn> [--arg <json>]
"""

import json
import subprocess
import sys
import threading


def main():
    if len(sys.argv) < 3:
        print("Usage: bridge_mock_host.py <harn_binary> <pipeline.harn> [--arg <json>]", file=sys.stderr)
        sys.exit(1)

    harn_binary = sys.argv[1]
    pipeline = sys.argv[2]
    extra_args = sys.argv[3:]

    cmd = [harn_binary, "run", "--bridge", pipeline] + extra_args

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    outputs = []
    errors = []

    def read_and_respond():
        """Read JSON-RPC messages from the harn process and respond."""
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                errors.append(f"Invalid JSON from VM: {line}")
                continue

            # Notification (no id) — just collect output
            if "id" not in msg:
                method = msg.get("method", "")
                if method == "output":
                    outputs.append(msg["params"]["text"])
                elif method == "progress":
                    pass  # ignore progress for testing
                elif method == "error":
                    errors.append(msg["params"].get("message", "unknown error"))
                continue

            # Request — respond based on method
            req_id = msg["id"]
            method = msg.get("method", "")
            params = msg.get("params", {})

            if method == "llm_call":
                prompt = params.get("prompt", "")
                response = {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "text": f"Mock LLM response to: {prompt}",
                        "input_tokens": 100,
                        "output_tokens": 50,
                    },
                }
            elif method == "tool_execute":
                tool_name = params.get("name", "")
                tool_args = params.get("arguments", {})
                if tool_name == "read_file":
                    response = {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {"content": f"Mock content of {tool_args.get('path', '?')}"},
                    }
                elif tool_name == "exec" or tool_name == "run_command":
                    response = {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {"output": "mock command output", "exit_code": 0},
                    }
                else:
                    response = {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {"ok": True},
                    }
            elif method == "host_call":
                name = params.get("name", "")
                response = {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": f"Mock host_call result for: {name}",
                }
            elif method == "agent_loop":
                prompt = params.get("prompt", "")
                response = {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "status": "done",
                        "text": f"Mock agent_loop result for: {prompt}",
                        "iterations": 1,
                        "duration_ms": 0,
                        "tools_used": [],
                    },
                }
            else:
                response = {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {"code": -32601, "message": f"Unknown method: {method}"},
                }

            proc.stdin.write(json.dumps(response) + "\n")
            proc.stdin.flush()

    reader_thread = threading.Thread(target=read_and_respond, daemon=True)
    reader_thread.start()

    proc.wait()

    # Print collected outputs
    for out in outputs:
        sys.stdout.write(out)

    if errors:
        for err in errors:
            print(f"ERROR: {err}", file=sys.stderr)
        sys.exit(1)

    sys.exit(proc.returncode)


if __name__ == "__main__":
    main()
