#!/usr/bin/env python3
"""Compute the affected-crate set for PR fast-feedback test selection.

Given a base git ref, this:

  1. diffs ``<base>...HEAD`` to get the changed files,
  2. maps each changed file to the workspace crate that owns it (via the
     crate directories reported by ``cargo metadata``),
  3. expands that set with the **reverse-dependency closure** — every crate
     that (transitively) depends on a directly-changed crate — because a
     change to crate X can break any crate that builds on top of X, and
  4. emits a ``cargo-nextest`` selection covering exactly those crates' tests.

This exists because ``cargo-nextest`` has no ``--changed-since``: the
selection has to be computed explicitly from the dependency graph.

Soundness note: this is a PR-only fast-feedback optimization. The merge
queue (``merge_group``) runs the FULL suite unconditionally — see
``.github/workflows/ci.yml``. Never wire this into that path. Push-to-main
CI is deliberately cheap because branch protection has already admitted the
merge-queued tree.

Changes to files that do NOT belong to any single crate (workspace
``Cargo.toml``/``Cargo.lock``, ``.cargo/``, ``rust-toolchain.toml``, the CI
workflow, this script, the Makefile, etc.) are treated as "global": they
force the full workspace, because they can affect every crate's build.

Output modes (``--output``):

  * ``filter`` (default): a single nextest ``-E`` filterset expression, e.g.
    ``package(harn-vm) or package(harn-cli)``. Prints nothing when the full
    workspace is selected (the caller should run ``--workspace``).
  * ``packages``: newline-separated crate names (one per line).
  * ``args``: the ``-p <crate>`` argument string nextest consumes directly
    (``-p harn-vm -p harn-cli ...``), or ``--workspace`` for the full set.
    Using ``-p`` flags prunes both compilation and test execution to the
    selected crates, which is where the PR fast-feedback win comes from
    (an ``-E`` filterset alone still compiles the whole workspace graph).

Exit status is always 0 on success; diagnostics go to stderr.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

# File path prefixes/names that, when changed, invalidate the whole workspace.
# Anything matching these is "global" — we can't soundly prune, so we run all.
GLOBAL_PATHS = (
    "Cargo.toml",  # workspace manifest (repo root)
    "Cargo.lock",
    ".cargo/",
    "rust-toolchain.toml",
    "Makefile",
    "scripts/affected-crates.py",
    ".config/nextest.toml",
    ".github/workflows/ci.yml",
)


def run(cmd: list[str]) -> str:
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def load_workspace() -> tuple[dict[str, Path], dict[str, set[str]]]:
    """Return (crate_name -> crate_dir, crate_name -> direct workspace deps)."""
    meta = json.loads(
        run(["cargo", "metadata", "--no-deps", "--format-version", "1"])
    )
    workspace_members = set(meta["workspace_members"])
    names_by_id = {p["id"]: p["name"] for p in meta["packages"]}
    workspace_names = {names_by_id[m] for m in workspace_members}

    crate_dir: dict[str, Path] = {}
    deps: dict[str, set[str]] = {}
    for pkg in meta["packages"]:
        name = pkg["name"]
        if name not in workspace_names:
            continue
        crate_dir[name] = Path(pkg["manifest_path"]).parent
        deps[name] = {
            d["name"] for d in pkg["dependencies"] if d["name"] in workspace_names
        }
    return crate_dir, deps


def changed_files(base: str) -> list[str]:
    """Files changed in ``<base>...HEAD`` (merge-base diff)."""
    out = run(["git", "diff", "--name-only", f"{base}...HEAD"])
    return [line for line in out.splitlines() if line.strip()]


def owning_crate(rel_path: str, crate_dir: dict[str, Path], root: Path) -> str | None:
    """Return the crate that owns ``rel_path``, or None if not under a crate."""
    abs_path = (root / rel_path).resolve()
    best: str | None = None
    best_len = -1
    for name, directory in crate_dir.items():
        d = directory.resolve()
        try:
            abs_path.relative_to(d)
        except ValueError:
            continue
        # Most-specific (longest) directory wins, so a file under a nested
        # crate isn't misattributed to a parent dir.
        depth = len(d.parts)
        if depth > best_len:
            best, best_len = name, depth
    return best


def reverse_dep_closure(seeds: set[str], deps: dict[str, set[str]]) -> set[str]:
    """Seeds plus every crate that transitively depends on a seed."""
    # Build the reverse edges: dependency -> set of crates that depend on it.
    rdeps: dict[str, set[str]] = {name: set() for name in deps}
    for name, direct in deps.items():
        for dep in direct:
            rdeps.setdefault(dep, set()).add(name)

    closure = set(seeds)
    frontier = list(seeds)
    while frontier:
        current = frontier.pop()
        for dependent in rdeps.get(current, ()):
            if dependent not in closure:
                closure.add(dependent)
                frontier.append(dependent)
    return closure


def emit(selected: set[str], all_crates: set[str], output: str) -> int:
    full = selected == all_crates
    if output == "packages":
        for name in sorted(selected):
            print(name)
        return 0

    if full:
        # Full workspace: tell the caller to run the whole workspace.
        if output == "args":
            print("--workspace")
        return 0

    if output == "filter":
        print(" or ".join(f"package({name})" for name in sorted(selected)))
    else:  # args
        print(" ".join(f"-p {name}" for name in sorted(selected)))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base",
        default="origin/main",
        help="base ref for the diff (default: origin/main)",
    )
    parser.add_argument(
        "--output",
        choices=("filter", "packages", "args"),
        default="filter",
        help="output format (default: filter)",
    )
    args = parser.parse_args()

    root = Path(run(["git", "rev-parse", "--show-toplevel"]).strip())
    crate_dir, deps = load_workspace()
    all_crates = set(crate_dir)

    files = changed_files(args.base)
    if not files:
        print(
            f"affected-crates: no files changed vs {args.base}; "
            "selecting nothing.",
            file=sys.stderr,
        )
        # Empty selection — caller treats this as "skip Rust tests". In
        # practice CI only reaches the test job when Rust files changed.
        return 0

    global_hits = [f for f in files if f.startswith(GLOBAL_PATHS)]
    if global_hits:
        unique = sorted(set(global_hits))
        print(
            "affected-crates: global/workspace-level change detected "
            f"({', '.join(unique[:5])}{' ...' if len(unique) > 5 else ''}); "
            "selecting the FULL workspace (no pruning).",
            file=sys.stderr,
        )
        return emit(all_crates, all_crates, args.output)

    directly_changed: set[str] = set()
    unowned: list[str] = []
    for f in files:
        crate = owning_crate(f, crate_dir, root)
        if crate is None:
            unowned.append(f)
        else:
            directly_changed.add(crate)

    if not directly_changed:
        # Changed files exist but none belong to a crate (e.g. only docs/
        # *.harn fixtures changed). Nothing to test at the Rust level.
        print(
            "affected-crates: changed files touch no Rust crate "
            f"(e.g. {', '.join(unowned[:3])}); selecting nothing.",
            file=sys.stderr,
        )
        return 0

    affected = reverse_dep_closure(directly_changed, deps)
    pruned = sorted(all_crates - affected)

    print(
        "affected-crates: directly changed: "
        + ", ".join(sorted(directly_changed)),
        file=sys.stderr,
    )
    print(
        "affected-crates: selected (changed + rdeps closure): "
        + ", ".join(sorted(affected)),
        file=sys.stderr,
    )
    print(
        "affected-crates: pruned (not selected): "
        + (", ".join(pruned) if pruned else "(none)"),
        file=sys.stderr,
    )
    if unowned:
        print(
            "affected-crates: note: non-crate files also changed "
            "(not test-relevant at the crate level): "
            + ", ".join(unowned[:8])
            + (" ..." if len(unowned) > 8 else ""),
            file=sys.stderr,
        )

    return emit(affected, all_crates, args.output)


if __name__ == "__main__":
    sys.exit(main())
