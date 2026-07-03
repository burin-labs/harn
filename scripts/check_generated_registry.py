#!/usr/bin/env python3
"""Audit that the generated-artifact registry agrees with its consumers."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path
from typing import Any

REGISTRY = "scripts/generated_artifacts.toml"
MAKEFILE = "Makefile"
WORKFLOWS_GLOB = ".github/workflows/*.yml"
AUDIT_GATE_RUNNER = "scripts/audit_gates.sh"
ASCII_WORD = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_"


def is_word_or_hyphen(char: str) -> bool:
    return char == "-" or char in ASCII_WORD


def whole_target_search(target: str, text: str) -> bool:
    if target == "":
        return False

    pos = 0
    while True:
        idx = text.find(target, pos)
        if idx == -1:
            return False
        end = idx + len(target)
        if end >= len(text) or not is_word_or_hyphen(text[end]):
            return True
        pos = idx + 1


def makefile_targets(text: str) -> list[str]:
    targets: list[str] = []
    seen: set[str] = set()
    for line in text.splitlines():
        if line.startswith("\t") or line.startswith(" "):
            continue
        match = re.match(r"^([A-Za-z][A-Za-z0-9_-]*):", line)
        if match is None:
            continue
        name = match.group(1)
        if name not in seen:
            seen.add(name)
            targets.append(name)
    return sorted(targets)


def make_all_recipe(text: str) -> str:
    body: list[str] = []
    in_all = False
    for line in text.splitlines():
        if not in_all:
            if line.startswith("all:"):
                in_all = True
            continue
        if line.startswith("\t"):
            body.append(line)
        elif line.strip() == "":
            continue
        else:
            break
    return "\n".join(body)


def artifact_flag_errors(
    art: dict[str, Any],
    check: str,
    all_recipe: str,
    workflow_text: str,
    missing_outputs: set[str],
) -> list[str]:
    aid = str(art.get("id", "<missing id>"))
    errors: list[str] = []

    ci = art.get("ci")
    if ci is True and check != "" and not whole_target_search(check, workflow_text):
        errors.append(
            f"[{aid}] ci = true but `{check}` is not referenced in any "
            ".github/workflows/*.yml -- drift here would not fail a PR. "
            f"Add `make {check}` to a workflow or set ci = false (with a note)."
        )
    if ci is False and check != "" and whole_target_search(check, workflow_text):
        errors.append(
            f"[{aid}] ci = false but `{check}` IS referenced in a workflow. "
            "Flip ci = true in the registry to reflect reality."
        )

    if art.get("make_all") is True and check != "" and not whole_target_search(check, all_recipe):
        errors.append(
            f"[{aid}] make_all = true but `{check}` is missing from the "
            "`all:` recipe in the Makefile."
        )

    for out in art.get("outputs", []):
        if out in missing_outputs:
            errors.append(f"[{aid}] declared output does not exist: {out}")
    return errors


def completeness_errors(
    targets: list[str],
    registered_gens: set[str],
    registered_checks: set[str],
    exempt: set[str],
) -> list[str]:
    errors: list[str] = []
    for target in targets:
        if target.startswith(("gen-", "sync-")) and target not in registered_gens:
            errors.append(
                f"Makefile target `{target}` generates an artifact but is not "
                f"registered in {REGISTRY} (add an [[artifact]] block with "
                f'gen = "{target}").'
            )
        elif (
            target.startswith("check-")
            and target not in registered_checks
            and target not in exempt
        ):
            errors.append(
                f"Makefile target `{target}` is a check but is neither "
                f"registered as an [[artifact]].check nor listed in "
                f"[meta].exempt_checks in {REGISTRY}."
            )
    return errors


def exempt_hygiene_errors(exempt: set[str], registered_checks: set[str]) -> list[str]:
    return [
        f"`{name}` is both registered as an artifact check and listed in "
        "[meta].exempt_checks -- remove it from exempt_checks."
        for name in sorted(exempt & registered_checks)
    ]


def validate(
    artifacts: list[dict[str, Any]],
    exempt: list[str],
    targets: list[str],
    all_recipe: str,
    workflow_text: str,
    missing_outputs: set[str],
) -> list[str]:
    errors: list[str] = []
    seen_ids: set[str] = set()
    registered_checks: set[str] = set()
    registered_gens: set[str] = set()
    target_set = set(targets)

    for art in artifacts:
        aid = str(art.get("id", "<missing id>"))
        if aid in seen_ids:
            errors.append(f"duplicate artifact id: {aid}")
        seen_ids.add(aid)

        check = str(art.get("check", ""))
        gen = str(art.get("gen", ""))
        if check == "":
            errors.append(f"[{aid}] missing required `check` target")
        else:
            registered_checks.add(check)
            if check not in target_set:
                errors.append(f"[{aid}] check target `{check}` is not defined in the Makefile")

        if gen != "":
            registered_gens.add(gen)
            if gen not in target_set:
                errors.append(f"[{aid}] gen target `{gen}` is not defined in the Makefile")

        errors.extend(
            artifact_flag_errors(art, check, all_recipe, workflow_text, missing_outputs)
        )

    exempt_set = set(exempt)
    errors.extend(completeness_errors(targets, registered_gens, registered_checks, exempt_set))
    errors.extend(exempt_hygiene_errors(exempt_set, registered_checks))
    return errors


def read_workflows(repo_root: Path) -> str:
    text = ""
    for workflow in sorted(repo_root.glob(WORKFLOWS_GLOB)):
        text += workflow.read_text(encoding="utf-8") + "\n"
    audit_runner = repo_root / AUDIT_GATE_RUNNER
    if audit_runner.exists():
        text += audit_runner.read_text(encoding="utf-8") + "\n"
    return text


def main() -> int:
    repo_root = Path(__file__).resolve().parents[1]
    try:
        registry = tomllib.loads((repo_root / REGISTRY).read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as err:
        print(f"error: failed to parse {REGISTRY}: {err}", file=sys.stderr)
        return 1

    artifacts = registry.get("artifact", [])
    exempt = registry.get("meta", {}).get("exempt_checks", [])
    make_text = (repo_root / MAKEFILE).read_text(encoding="utf-8")
    targets = makefile_targets(make_text)
    all_recipe = make_all_recipe(make_text)
    workflow_text = read_workflows(repo_root)
    missing_outputs = {
        out
        for art in artifacts
        for out in art.get("outputs", [])
        if not (repo_root / out).exists()
    }

    errors = validate(artifacts, exempt, targets, all_recipe, workflow_text, missing_outputs)
    if errors:
        print("error: generated-artifact registry is out of sync:\n", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        print(
            "\nhint: edit scripts/generated_artifacts.toml (and the Makefile / "
            "workflows it points at) until they agree. See the header of that "
            "file for the add-a-new-artifact checklist.",
            file=sys.stderr,
        )
        return 1

    print(
        f"    Generated-artifact registry OK ({len(artifacts)} artifacts, "
        f"{len(exempt)} exempt checks)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
