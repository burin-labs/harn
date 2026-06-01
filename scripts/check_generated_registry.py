#!/usr/bin/env python3
"""Audit that the generated-artifact registry agrees with its consumers.

`scripts/generated_artifacts.toml` is the single source of truth for every
"source of truth -> generated/mirrored file + drift check" pair in the repo.
This script (run via `make check-generated-registry`) fails the build when
the registry and the things that are supposed to track it have drifted:

  * a `gen-*` / `sync-*` Makefile target with no registry entry
    (someone added a generated artifact but never registered it);
  * a `check-*` Makefile target that is neither registered nor explicitly
    exempted (a new drift guard that could silently escape the audit);
  * a registry entry naming a Makefile target that does not exist;
  * a registry entry flagged `ci = true` whose check is not referenced in
    any `.github/workflows/*.yml` (drift would not fail any PR);
  * a registry entry flagged `make_all = true` whose check is missing from
    the `all:` recipe (so `make all` would not catch it);
  * a declared output file that does not exist on disk.

It is intentionally pure-Python with no Rust/`harn` dependency so it runs in
the fast `audit-scripts` CI lane and in git hooks without a build.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
REGISTRY = REPO_ROOT / "scripts" / "generated_artifacts.toml"
MAKEFILE = REPO_ROOT / "Makefile"
WORKFLOWS_DIR = REPO_ROOT / ".github" / "workflows"

# Match a Makefile rule definition: `target-name:` at column 0.
RULE_RE = re.compile(r"^([A-Za-z][A-Za-z0-9_-]*):")


def whole_target(target: str) -> re.Pattern[str]:
    """Match `target` not followed by another `-segment` or word char.

    Prevents `check-provider-catalog` from matching inside
    `check-provider-catalog-drift`.
    """
    return re.compile(re.escape(target) + r"(?![\w-])")


def makefile_targets(text: str) -> set[str]:
    targets: set[str] = set()
    for line in text.splitlines():
        if line.startswith(("\t", " ")):
            continue
        m = RULE_RE.match(line)
        if m:
            targets.add(m.group(1))
    return targets


def make_all_recipe(text: str) -> str:
    """Return the recipe body of the `all:` target (its tab-indented lines)."""
    lines = text.splitlines()
    body: list[str] = []
    in_all = False
    for line in lines:
        if line.startswith("all:"):
            in_all = True
            continue
        if in_all:
            if line.startswith("\t"):
                body.append(line)
            elif line.strip() == "":
                continue
            else:
                break
    return "\n".join(body)


def main() -> int:
    errors: list[str] = []

    with REGISTRY.open("rb") as fh:
        registry = tomllib.load(fh)

    artifacts = registry.get("artifact", [])
    exempt = set(registry.get("meta", {}).get("exempt_checks", []))

    make_text = MAKEFILE.read_text(encoding="utf-8")
    targets = makefile_targets(make_text)
    all_recipe = make_all_recipe(make_text)

    workflow_text = ""
    for wf in sorted(WORKFLOWS_DIR.glob("*.yml")):
        workflow_text += wf.read_text(encoding="utf-8") + "\n"

    # --- Per-entry structural validation -----------------------------------
    seen_ids: set[str] = set()
    registered_checks: set[str] = set()
    registered_gens: set[str] = set()

    for art in artifacts:
        aid = art.get("id", "<missing id>")
        if aid in seen_ids:
            errors.append(f"duplicate artifact id: {aid}")
        seen_ids.add(aid)

        check = art.get("check", "")
        gen = art.get("gen", "")
        if not check:
            errors.append(f"[{aid}] missing required `check` target")
        else:
            registered_checks.add(check)
            if check not in targets:
                errors.append(
                    f"[{aid}] check target `{check}` is not defined in the Makefile"
                )
        if gen:
            registered_gens.add(gen)
            if gen not in targets:
                errors.append(
                    f"[{aid}] gen target `{gen}` is not defined in the Makefile"
                )

        # ci flag must be honoured by a real workflow reference.
        if art.get("ci") and check and not whole_target(check).search(workflow_text):
            errors.append(
                f"[{aid}] ci = true but `{check}` is not referenced in any "
                f".github/workflows/*.yml — drift here would not fail a PR. "
                f"Add `make {check}` to a workflow or set ci = false (with a note)."
            )
        # Defensive: a check marked ci=false should not silently be in CI.
        if art.get("ci") is False and check and whole_target(check).search(workflow_text):
            errors.append(
                f"[{aid}] ci = false but `{check}` IS referenced in a workflow. "
                f"Flip ci = true in the registry to reflect reality."
            )

        # make_all flag must be honoured by the `all:` recipe.
        if art.get("make_all") and check and not whole_target(check).search(all_recipe):
            errors.append(
                f"[{aid}] make_all = true but `{check}` is missing from the "
                f"`all:` recipe in the Makefile."
            )

        # Declared outputs must exist.
        for out in art.get("outputs", []):
            if not (REPO_ROOT / out).exists():
                errors.append(f"[{aid}] declared output does not exist: {out}")

    # --- Completeness: no gen/check pair may escape the registry -----------
    for target in sorted(targets):
        if target.startswith(("gen-", "sync-")):
            if target not in registered_gens:
                errors.append(
                    f"Makefile target `{target}` generates an artifact but is not "
                    f"registered in scripts/generated_artifacts.toml (add an "
                    f"[[artifact]] block with gen = \"{target}\")."
                )
        elif (
            target.startswith("check-")
            and target not in registered_checks
            and target not in exempt
        ):
            errors.append(
                f"Makefile target `{target}` is a check but is neither "
                f"registered as an [[artifact]].check nor listed in "
                f"[meta].exempt_checks in scripts/generated_artifacts.toml."
            )

    # Exempt list hygiene: don't exempt something that is also registered.
    for ex in sorted(exempt & registered_checks):
        errors.append(
            f"`{ex}` is both registered as an artifact check and listed in "
            f"[meta].exempt_checks — remove it from exempt_checks."
        )

    if errors:
        print("error: generated-artifact registry is out of sync:\n", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        print(
            "\nhint: edit scripts/generated_artifacts.toml (and the Makefile / "
            "workflows it points at) until they agree. See the header of that "
            "file for the add-a-new-artifact checklist.",
            file=sys.stderr,
        )
        return 1

    print(
        f"    Generated-artifact registry OK "
        f"({len(artifacts)} artifacts, {len(exempt)} exempt checks)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
