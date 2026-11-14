"""Independent checks for an executed resource generator, outside its package."""

from __future__ import annotations

import hashlib
import importlib
import json
from pathlib import Path
import subprocess
import sys


def census(root: Path) -> dict[str, str]:
    return {
        str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(root.rglob("*"))
        if path.is_file()
        and not any(part in {"__pycache__", ".harn", ".harn-runs", ".burin"}
                    for part in path.relative_to(root).parts)
    }


def assert_response(app, method: str, path: str, payload, status: int):
    actual_status, body = app.dispatch(method, path, payload)
    assert actual_status == status, (method, path, payload, actual_status, body)
    return body


def check_requests(case: dict) -> int:
    app_module = importlib.import_module("app.http")
    app = app_module.App()
    other = app_module.App()
    route = f'/api/{case["resource"]}'
    fields = case["fields"]
    values = {"string": 'A "quoted" value\nwith Unicode: café',
              "integer": -7, "boolean": False}
    payload = {name: values[kind] for name, kind in fields.items()}
    checked = 0
    assert assert_response(app, "GET", route, None, 200) == []
    assert assert_response(app, "GET", "/health", None, 200) == {"status": "ok"}
    for name, kind in fields.items():
        missing = payload.copy()
        del missing[name]
        assert_response(app, "POST", route, missing, 400)
        checked += 1
        for wrong in [None, [], {}, 0.5, "wrong", 12, True, False]:
            expected_type = {"string": str, "integer": int, "boolean": bool}[kind]
            if type(wrong) is expected_type:
                continue
            assert_response(app, "POST", route, {**payload, name: wrong}, 400)
            assert assert_response(app, "GET", route, None, 200) == []
            checked += 1
    assert_response(app, "POST", route, {**payload, "unexpected_field": 1}, 400)
    assert assert_response(app, "GET", route, None, 200) == []
    first = {**payload, "id": 1}
    second_payload = {name: {"string": "", "integer": 0, "boolean": True}[kind]
                      for name, kind in fields.items()}
    second = {**second_payload, "id": 2}
    assert assert_response(app, "POST", route, payload, 201) == first
    assert assert_response(app, "POST", route, second_payload, 201) == second
    assert assert_response(app, "GET", route, None, 200) == [first, second]
    assert assert_response(other, "GET", route, None, 200) == []
    assert assert_response(other, "POST", route, payload, 201) == first
    assert assert_response(app, "GET", route, None, 200) == [first, second]
    return checked + 11


def grade(root: Path, base: Path, case: dict) -> dict:
    before = census(base)
    after = census(root)
    resource = case["resource"]
    outputs = {f"app/resources/{resource}.py", f"tests/test_{resource}.py",
               f"docs/{resource}.md"}
    assert all(after.get(path) == digest for path, digest in before.items()), "base changed"
    added = set(after) - set(before)
    assert added == outputs, {"missing": sorted(outputs - added),
                              "unexpected": sorted(added - outputs)}
    sys.path.insert(0, str(root))
    checked = check_requests(case)
    tests = subprocess.run(
        [sys.executable, "-m", "unittest", "discover", "-s", "tests", "-v"],
        cwd=root, capture_output=True, text=True, timeout=30,
    )
    assert tests.returncode == 0, tests.stderr
    # Prove the generated test is discovered and exercises this resource.
    assert f"test_{resource}." in tests.stderr, "generated tests were not discovered"
    docs = root.joinpath(f"docs/{resource}.md").read_text()
    assert f"/api/{resource}" in docs and "POST" in docs and "GET" in docs
    assert all(field in docs for field in case["fields"]), "undocumented fields"
    assert "201" in docs and "400" in docs, "undocumented response states"
    return {"kind": "passed", "request_checks": checked,
            "generated_tests_discovered": True, "changed_paths": sorted(outputs),
            "workspace_digest": hashlib.sha256(
                json.dumps(after, sort_keys=True).encode()).hexdigest()}


if __name__ == "__main__":
    try:
        result = grade(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(),
                       json.loads(Path(sys.argv[3]).read_text()))
    except Exception as error:
        result = {"kind": "failed", "error": f"{type(error).__name__}: {error}"}
    print(json.dumps(result, sort_keys=True))
    raise SystemExit(0 if result["kind"] == "passed" else 1)
