#!/usr/bin/env python3
"""Prove every Blacksmith-capable job agrees with its declared runner tier.

Background
----------
Blacksmith runners register with GitHub as ordinary *self-hosted* runners.
`runner.environment` therefore reports `self-hosted` on them, identically to
our own boxes, and cannot distinguish the two. Any job that can land on
Blacksmith and branches on `runner.environment` silently takes the wrong
branch there: `== 'self-hosted'` guards fire on vendor capacity and
`!= 'self-hosted'` guards skip the steps they were meant to run. That is how
sticky-disk mounts, dependency caches and core-count tuning end up pointed at
the wrong machine (observed on the burin-code Rust TUI lanes, burin-code#5423).

The fix is one owner per job: a job-level `HARN_RUNNER_TIER` env var that
mirrors the `runs-on` ladder and resolves to exactly one of TIERS. Steps
branch on that instead. This gate keeps the two halves honest.

Checks
------
1. No Blacksmith-capable job evaluates `runner.environment` anywhere.
2. A job that reads `env.HARN_RUNNER_TIER` must declare it at job level.
3. A declared tier must only ever produce a value in TIERS.
4. The tier and the `runs-on` ladder must agree -- proved by evaluating BOTH
   expressions over every combination of the context values they read, and
   asserting the runner selected always matches the tier reported. This is an
   exhaustive proof over the reachable input space, not a textual diff, so
   reordering or restructuring either expression is fine as long as the two
   still denote the same ladder.

Exit 0 when clean, 1 with a per-violation report otherwise.
"""

from __future__ import annotations

import itertools
import json
import re
import sys
from pathlib import Path
from typing import Any

TIER_ENV = "HARN_RUNNER_TIER"
TIERS = ("self-hosted", "blacksmith", "github-hosted")
BLACKSMITH_RUNNER_PREFIX = "blacksmith-"
SELF_HOSTED_LABEL = "self-hosted"
# Stands in for "any value the workflow never compares against", so the
# enumeration below covers the fall-through arm of every predicate.
OTHER = "\x00other"

WORKFLOW_DIR = Path(".github/workflows")


# --------------------------------------------------------------------------
# A small evaluator for the subset of GitHub expression syntax these ladders
# use. `&&` and `||` return an *operand*, not a boolean -- that value-returning
# behaviour is the whole mechanism behind the `cond && 'a' || 'b'` idiom, so it
# has to be modelled exactly rather than approximated with Python's bool.
# --------------------------------------------------------------------------

TOKEN_RE = re.compile(
    r"""
    \s*(?:
        (?P<string>'(?:[^']|'')*')
      | (?P<op>&&|\|\||==|!=|\(|\)|,|!)
      | (?P<ident>[A-Za-z_][A-Za-z0-9_.\-]*)
      | (?P<number>[0-9]+)
    )
    """,
    re.VERBOSE,
)


class ExprError(Exception):
    pass


def tokenize(src: str) -> list[tuple[str, str]]:
    tokens: list[tuple[str, str]] = []
    pos = 0
    while pos < len(src):
        if src[pos].isspace():
            pos += 1
            continue
        match = TOKEN_RE.match(src, pos)
        if not match or match.end() == pos:
            raise ExprError(f"cannot tokenize at {src[pos:pos + 30]!r}")
        kind = match.lastgroup
        assert kind is not None
        tokens.append((kind, match.group(kind).strip()))
        pos = match.end()
    return tokens


class Parser:
    def __init__(self, tokens: list[tuple[str, str]]) -> None:
        self.tokens = tokens
        self.pos = 0

    def peek(self) -> tuple[str, str] | None:
        return self.tokens[self.pos] if self.pos < len(self.tokens) else None

    def eat(self, value: str) -> bool:
        token = self.peek()
        if token and token[1] == value:
            self.pos += 1
            return True
        return False

    def expect(self, value: str) -> None:
        if not self.eat(value):
            raise ExprError(f"expected {value!r} at token {self.pos}")

    def parse(self) -> Any:
        node = self.parse_or()
        if self.peek() is not None:
            raise ExprError(f"trailing tokens from {self.pos}")
        return node

    def parse_or(self) -> Any:
        node = self.parse_and()
        while self.eat("||"):
            node = ("or", node, self.parse_and())
        return node

    def parse_and(self) -> Any:
        node = self.parse_cmp()
        while self.eat("&&"):
            node = ("and", node, self.parse_cmp())
        return node

    def parse_cmp(self) -> Any:
        node = self.parse_unary()
        while True:
            token = self.peek()
            if token and token[1] in ("==", "!="):
                self.pos += 1
                node = (token[1], node, self.parse_unary())
            else:
                return node

    def parse_unary(self) -> Any:
        if self.eat("!"):
            return ("not", self.parse_unary())
        return self.parse_primary()

    def parse_primary(self) -> Any:
        token = self.peek()
        if token is None:
            raise ExprError("unexpected end of expression")
        kind, value = token
        if value == "(":
            self.pos += 1
            node = self.parse_or()
            self.expect(")")
            return node
        if kind == "string":
            self.pos += 1
            return ("lit", value[1:-1].replace("''", "'"))
        if kind == "number":
            self.pos += 1
            return ("lit", int(value))
        if kind == "ident":
            self.pos += 1
            if self.eat("("):
                args = [self.parse_or()]
                while self.eat(","):
                    args.append(self.parse_or())
                self.expect(")")
                return ("call", value, args)
            if value == "true":
                return ("lit", True)
            if value == "false":
                return ("lit", False)
            if value == "null":
                return ("lit", None)
            return ("path", value)
        raise ExprError(f"unexpected token {value!r}")


def truthy(value: Any) -> bool:
    if value is None or value is False:
        return False
    if value == "" or value == 0:
        return False
    return True


def eq(left: Any, right: Any) -> bool:
    # GitHub coerces loosely; every comparison in these ladders is
    # string-vs-string or bool-vs-string, so normalise booleans to their
    # lowercase spelling and compare as text.
    def norm(value: Any) -> Any:
        if isinstance(value, bool):
            return "true" if value else "false"
        if value is None:
            return ""
        return value

    return norm(left) == norm(right)


def evaluate(node: Any, ctx: dict[str, Any]) -> Any:
    kind = node[0]
    if kind == "lit":
        return node[1]
    if kind == "path":
        return ctx.get(node[1])
    if kind == "and":
        left = evaluate(node[1], ctx)
        return evaluate(node[2], ctx) if truthy(left) else left
    if kind == "or":
        left = evaluate(node[1], ctx)
        return left if truthy(left) else evaluate(node[2], ctx)
    if kind == "not":
        return not truthy(evaluate(node[1], ctx))
    if kind in ("==", "!="):
        result = eq(evaluate(node[1], ctx), evaluate(node[2], ctx))
        return result if kind == "==" else not result
    if kind == "call":
        name, args = node[1], node[2]
        if name == "fromJSON":
            return json.loads(evaluate(args[0], ctx))
        raise ExprError(f"unsupported function {name!r}")
    raise ExprError(f"unsupported node {kind!r}")


EXPR_RE = re.compile(r"\$\{\{(.*?)\}\}", re.DOTALL)


def sole_expression(text: str) -> str | None:
    """Return the single `${{ ... }}` body filling `text`, else None."""
    if not isinstance(text, str):
        return None
    matches = EXPR_RE.findall(text)
    if len(matches) != 1:
        return None
    if EXPR_RE.sub("", text).strip():
        return None  # interpolated into surrounding literal text
    return matches[0].strip()


def paths_in(node: Any, out: set[str]) -> None:
    if node[0] == "path":
        out.add(node[1])
    elif node[0] in ("and", "or", "==", "!="):
        paths_in(node[1], out)
        paths_in(node[2], out)
    elif node[0] == "not":
        paths_in(node[1], out)
    elif node[0] == "call":
        for arg in node[2]:
            paths_in(arg, out)


def compared_literals(node: Any, out: dict[str, set[Any]]) -> None:
    """Collect, per context path, every literal it is compared against."""
    if node[0] in ("==", "!="):
        left, right = node[1], node[2]
        for a, b in ((left, right), (right, left)):
            if a[0] == "path" and b[0] == "lit":
                out.setdefault(a[1], set()).add(b[1])
    for child in node[1:]:
        if isinstance(child, tuple):
            compared_literals(child, out)
        elif isinstance(child, list):
            for item in child:
                compared_literals(item, out)


def classify_runner(value: Any) -> str | None:
    """Map a resolved `runs-on` value onto the tier it implies."""
    if isinstance(value, list):
        return SELF_HOSTED_LABEL if SELF_HOSTED_LABEL in value else None
    if isinstance(value, str):
        if value == SELF_HOSTED_LABEL:
            return SELF_HOSTED_LABEL
        if value.startswith(BLACKSMITH_RUNNER_PREFIX):
            return "blacksmith"
        if value:
            return "github-hosted"
    return None


def uses_runner_environment(node: Any) -> bool:
    found: set[str] = set()
    paths_in(node, found)
    return any(path.startswith("runner.environment") for path in found)


def walk_expressions(value: Any, path: str = "", *, bare: bool = False):
    """Yield (location, expression-body) for every expression in a YAML subtree.

    `if:` is the one key whose value is an expression even without the `${{ }}`
    wrapper -- `if: runner.environment == 'self-hosted'` is evaluated, not a
    literal string. Missing that is how the first version of this gate saw only
    6 of the 14 known-bad sites.
    """
    if isinstance(value, dict):
        for key, child in value.items():
            child_path = f"{path}.{key}" if path else str(key)
            yield from walk_expressions(child, child_path, bare=(key == "if"))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_expressions(child, f"{path}[{index}]")
    elif isinstance(value, str):
        wrapped = EXPR_RE.findall(value)
        if wrapped:
            for body in wrapped:
                yield path, body.strip()
        elif bare and value.strip():
            yield path, value.strip()


def matrix_includes(job: dict[str, Any]) -> list[dict[str, Any]]:
    include = (job.get("strategy") or {}).get("matrix", {})
    if isinstance(include, dict) and isinstance(include.get("include"), list):
        return [entry for entry in include["include"] if isinstance(entry, dict)]
    return [{}]


def check_job(name: str, job: dict[str, Any], errors: list[str]) -> None:
    runs_on = job.get("runs-on")
    runs_on_expr = sole_expression(runs_on)
    job_env = job.get("env") or {}
    tier_raw = job_env.get(TIER_ENV) if isinstance(job_env, dict) else None
    tier_expr = sole_expression(tier_raw) if tier_raw is not None else None

    includes = matrix_includes(job)

    # Does this job's ladder reach Blacksmith on any matrix leg?
    blacksmith_capable = False
    runs_on_ast = None
    if runs_on_expr:
        try:
            runs_on_ast = Parser(tokenize(runs_on_expr)).parse()
        except ExprError as exc:
            errors.append(f"{name}: cannot parse `runs-on`: {exc}")
            return
    if BLACKSMITH_RUNNER_PREFIX in str(runs_on):
        blacksmith_capable = True
    else:
        for entry in includes:
            if any(
                isinstance(v, str) and v.startswith(BLACKSMITH_RUNNER_PREFIX)
                for v in entry.values()
            ):
                blacksmith_capable = True

    # ---- Check 1: no `runner.environment` on a Blacksmith-capable job.
    if blacksmith_capable:
        for location, body in walk_expressions(job):
            if "runner.environment" in body:
                errors.append(
                    f"{name}.{location}: uses `runner.environment` on a "
                    f"Blacksmith-capable job. Blacksmith registers as "
                    f"self-hosted, so this cannot tell vendor capacity from our "
                    f"own boxes -- branch on `env.{TIER_ENV}` instead."
                )

    # ---- Check 2: reading the tier requires declaring it.
    reads_tier = any(
        f"env.{TIER_ENV}" in body
        for _, body in walk_expressions({k: v for k, v in job.items() if k != "env"})
    )
    if reads_tier and tier_expr is None:
        errors.append(
            f"{name}: reads `env.{TIER_ENV}` but does not declare it as a "
            f"job-level env var."
        )
    if tier_expr is None:
        return

    try:
        tier_ast = Parser(tokenize(tier_expr)).parse()
    except ExprError as exc:
        errors.append(f"{name}: cannot parse `{TIER_ENV}`: {exc}")
        return

    if uses_runner_environment(tier_ast):
        errors.append(f"{name}: `{TIER_ENV}` must not derive from `runner.environment`.")

    if runs_on_ast is None:
        errors.append(
            f"{name}: declares `{TIER_ENV}` but `runs-on` is not a single "
            f"expression, so the two cannot be proved to agree."
        )
        return

    # ---- Checks 3 & 4: exhaustively evaluate both expressions together.
    literals: dict[str, set[Any]] = {}
    compared_literals(runs_on_ast, literals)
    compared_literals(tier_ast, literals)

    free_paths: set[str] = set()
    paths_in(runs_on_ast, free_paths)
    paths_in(tier_ast, free_paths)

    for entry in includes:
        bound = {f"matrix.{k}": v for k, v in entry.items()}
        variable = sorted(p for p in free_paths if p not in bound)
        domains = [sorted(literals.get(p, set()) | {OTHER}, key=repr) for p in variable]

        for combo in itertools.product(*domains) if variable else [()]:
            ctx = dict(bound)
            ctx.update(dict(zip(variable, combo)))
            try:
                runner = evaluate(runs_on_ast, ctx)
                tier = evaluate(tier_ast, ctx)
            except (ExprError, json.JSONDecodeError) as exc:
                errors.append(f"{name}: evaluation failed: {exc}")
                return

            if tier not in TIERS:
                errors.append(
                    f"{name}: `{TIER_ENV}` resolves to {tier!r}, which is not "
                    f"one of {list(TIERS)} (inputs: {describe(ctx, variable)})."
                )
                return

            expected = classify_runner(runner)
            if expected is None:
                errors.append(
                    f"{name}: `runs-on` resolves to {runner!r}, which is not a "
                    f"recognisable runner (inputs: {describe(ctx, variable)})."
                )
                return
            if expected != tier:
                leg = f" [matrix {entry.get('kind') or entry}]" if bound else ""
                errors.append(
                    f"{name}{leg}: ladder disagrees with tier -- `runs-on` "
                    f"selects {runner!r} (tier {expected!r}) but `{TIER_ENV}` "
                    f"reports {tier!r} (inputs: {describe(ctx, variable)})."
                )
                return


def describe(ctx: dict[str, Any], variable: list[str]) -> str:
    return ", ".join(
        f"{p}={'<other>' if ctx[p] == OTHER else ctx[p]!r}" for p in variable
    )


def main() -> int:
    import yaml  # hard dependency on purpose: a skipped gate is a false green

    errors: list[str] = []
    checked = 0
    workflows = sorted(WORKFLOW_DIR.glob("*.yml")) + sorted(WORKFLOW_DIR.glob("*.yaml"))
    if not workflows:
        print(f"error: no workflows found under {WORKFLOW_DIR}", file=sys.stderr)
        return 1

    for workflow in workflows:
        document = yaml.safe_load(workflow.read_text())
        if not isinstance(document, dict):
            continue
        for name, job in (document.get("jobs") or {}).items():
            if not isinstance(job, dict) or "uses" in job:
                continue
            checked += 1
            check_job(f"{workflow.name}:{name}", job, errors)

    if errors:
        print("Runner-tier contract violations:\n", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        print(
            f"\n{len(errors)} violation(s). See scripts/check_runner_tier.py for why "
            f"`runner.environment` cannot answer this question.",
            file=sys.stderr,
        )
        return 1

    print(f"runner-tier contract OK ({checked} jobs checked).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
