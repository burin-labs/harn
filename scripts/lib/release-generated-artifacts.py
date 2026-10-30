#!/usr/bin/env python3
"""Project release regeneration commands and conflict scope from the artifact registry."""

import argparse
from pathlib import Path, PurePosixPath
import re
import tomllib


def release_artifacts(registry):
    artifacts = []
    for row in tomllib.loads(registry.read_text())["artifact"]:
        if "release_phase" not in row:
            continue
        if row["release_phase"] not in ("before-version", "after-version"):
            raise ValueError(f"invalid release phase for {row.get('id')}")
        if not isinstance(row.get("gen"), str) or not re.fullmatch(r"[a-z][a-z0-9-]*", row["gen"]):
            raise ValueError(f"missing release generator for {row.get('id')}")
        outputs = row.get("outputs")
        if not isinstance(outputs, list) or not outputs:
            raise ValueError(f"missing release outputs for {row.get('id')}")
        for output in outputs:
            if not isinstance(output, str) or not output or "\n" in output:
                raise ValueError(f"invalid release output for {row.get('id')}")
            path = PurePosixPath(output)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError(f"unrooted release output for {row.get('id')}")
        artifacts.append(row)
    if not artifacts:
        raise ValueError("release artifact census is empty")
    return artifacts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("targets", "matches"))
    parser.add_argument("value")
    parser.add_argument("--registry", type=Path, default=Path(__file__).resolve().parents[1] / "generated_artifacts.toml")
    options = parser.parse_args()
    artifacts = release_artifacts(options.registry)
    if options.action == "matches":
        return 0 if any(
            options.value == output or (output.endswith("/") and options.value.startswith(output))
            for row in artifacts for output in row["outputs"]
        ) else 1
    targets = [row["gen"] for row in artifacts if row["release_phase"] == options.value]
    if not targets:
        raise ValueError(f"release phase {options.value!r} has no generators")
    print("\n".join(targets))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
