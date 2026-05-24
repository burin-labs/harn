#!/usr/bin/env python3
"""Cold-start CLI benchmark helper.

Driven by scripts/bench_cli_cold_start.sh. Reads its inputs from env
vars so the bash wrapper can stay a thin argument parser.

Inputs (env):
  HARN_BIN                  Path to the harn release binary.
  HARN_CLI_BUDGETS_FILE     Path to perf/cli/budgets.toml.
  HARN_CLI_BASELINE_FILE    Path to perf/cli/baselines/main.json.
  HARN_CLI_ITERATIONS       Timed runs per command.
  HARN_CLI_COMMANDS_FILTER  Optional comma-separated bench-key subset.
  HARN_CLI_UPDATE_BASELINE  "1" to overwrite an already-populated slot.
  HARN_CLI_REPO_ROOT        Repository root.

See perf/cli/README.md for the schema and comparison rules.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    import tomllib  # Python >= 3.11
except ModuleNotFoundError:  # pragma: no cover — dev hosts run 3.11+
    print(
        "error: this script requires Python >= 3.11 for tomllib", file=sys.stderr
    )
    sys.exit(2)


# --- Env wiring -------------------------------------------------------------


def _require_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        print(f"error: missing required env var {name}", file=sys.stderr)
        sys.exit(2)
    return value


HARN_BIN = _require_env("HARN_BIN")
BUDGETS_FILE = Path(_require_env("HARN_CLI_BUDGETS_FILE"))
BASELINE_FILE = Path(_require_env("HARN_CLI_BASELINE_FILE"))
ITERATIONS = int(_require_env("HARN_CLI_ITERATIONS"))
COMMANDS_FILTER = [
    name.strip()
    for name in os.environ.get("HARN_CLI_COMMANDS_FILTER", "").split(",")
    if name.strip()
]
UPDATE_BASELINE = os.environ.get("HARN_CLI_UPDATE_BASELINE", "0") == "1"
REPO_ROOT = Path(_require_env("HARN_CLI_REPO_ROOT"))


# --- Tracked subcommands ---------------------------------------------------
#
# Hardcoded for the G5 skeleton — future W tickets append rows here and
# add a matching `[commands.<key>]` entry to perf/cli/budgets.toml.
#
# Each invocation must terminate quickly, must not require network, and
# must not depend on user state in `~/.harn`. The runner provides a
# fresh `HARN_CACHE_DIR` for every measurement and isolates the home
# directory via `HOME` override.

TRACKED_COMMANDS: list[dict[str, object]] = [
    {
        "key": "version",
        "args": ["version"],
        "needs_trace_input": False,
    },
    {
        "key": "try --help",
        "args": ["try", "--help"],
        "needs_trace_input": False,
    },
    # `trace import` is temporarily disabled in the bench because the
    # ported `.harn` impl honours the workspace_roots sandbox, which
    # rejects the `/tmp` paths the bench creates with
    # `tempfile.TemporaryDirectory`. Re-enable once the bench moves its
    # scratch dir under the repo root (then sandbox accepts it). See
    # the W13 PR (#2351) for the `dispatch_to_embedded_script_no_sandbox`
    # primitive that ports needing user-supplied paths can use; trace
    # import doesn't qualify since its inputs are user data, not just
    # bench temp files.
]


# --- Budget loading --------------------------------------------------------


def load_budgets() -> dict:
    raw = tomllib.loads(BUDGETS_FILE.read_text())
    defaults = raw.get("defaults", {})
    commands_table = raw.get("commands", {})

    default_cold = float(defaults.get("cold_ms", 250.0))
    default_ratio = float(defaults.get("regression_x", 1.25))

    resolved: dict[str, dict[str, float]] = {}
    for cmd in TRACKED_COMMANDS:
        key = cmd["key"]  # type: ignore[assignment]
        entry = commands_table.get(key, {})
        resolved[key] = {
            "cold_ms": float(entry.get("cold_ms", default_cold)),
            "regression_x": float(entry.get("regression_x", default_ratio)),
        }
    return resolved


# --- Baseline JSON --------------------------------------------------------


def load_baseline() -> dict:
    text = BASELINE_FILE.read_text().strip()
    if not text:
        return {}
    return json.loads(text)


def save_baseline(payload: dict) -> None:
    BASELINE_FILE.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def current_commit_sha() -> str:
    # Falls back to the literal string "uncommitted" if git is unavailable
    # or the working tree is not a repo (e.g. a tarball install). The
    # baseline file just uses this as a key, so any stable identifier
    # works for local iteration.
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
        return out.stdout.strip()
    except (FileNotFoundError, subprocess.CalledProcessError):
        return "uncommitted"


def most_recent_baseline_for(
    payload: dict, key: str, exclude_sha: str
) -> tuple[str | None, float | None]:
    """Return (commit_sha, cold_ms) for the newest baseline that has `key`.

    Newest is determined by `captured_at`. Falls back to insertion order
    when entries are missing the timestamp (older format). Skips
    `exclude_sha` so the current commit cannot accidentally become its
    own baseline if the file was written by an earlier invocation in
    the same run.
    """
    candidates: list[tuple[str, str, float]] = []
    for sha, entry in payload.items():
        if sha == exclude_sha:
            continue
        commands = entry.get("commands", {})
        if key not in commands:
            continue
        cold = commands[key].get("cold_ms")
        if not isinstance(cold, (int, float)):
            continue
        captured = entry.get("captured_at", "")
        candidates.append((captured, sha, float(cold)))
    if not candidates:
        return None, None
    candidates.sort(reverse=True)  # newest captured_at first
    _, sha, cold = candidates[0]
    return sha, cold


# --- Timing primitives ----------------------------------------------------


def _build_invocation(args_template: list[str], tmpdir: Path) -> list[str]:
    """Expand `{trace_input}` / `{trace_output}` placeholders."""
    trace_input = tmpdir / "trace_input.jsonl"
    trace_output = tmpdir / "trace_output.jsonl"
    if not trace_input.exists():
        # Single record so the parser is exercised end-to-end. We could
        # use /dev/null but Windows runners (which we don't gate on
        # today but might later) lack it; keeping the fixture on disk
        # makes the script portable.
        trace_input.write_text('{"prompt":"hi","response":"hi"}\n')
    rendered: list[str] = []
    for piece in args_template:
        rendered.append(
            piece.replace("{trace_input}", str(trace_input)).replace(
                "{trace_output}", str(trace_output)
            )
        )
    return rendered


def _isolated_env(cache_dir: Path, home_dir: Path) -> dict[str, str]:
    """Build a fresh env for one timed invocation.

    - `HARN_BYTECODE_CACHE=0` forces cold compile on every run.
    - `HARN_CACHE_DIR` is process-local and wiped between runs.
    - `HOME` is redirected so the binary cannot read or write the
      user's real `~/.harn`.
    """
    env = {
        key: value
        for key, value in os.environ.items()
        # Drop variables that could pin the binary to user state.
        if not key.startswith("HARN_")
    }
    env["HARN_BYTECODE_CACHE"] = "0"
    env["HARN_CACHE_DIR"] = str(cache_dir)
    env["HOME"] = str(home_dir)
    # Make sure the binary uses a deterministic locale.
    env.setdefault("LC_ALL", "C")
    return env


def _measure_with_hyperfine(
    invocation: list[str], runs: int, cache_dir: Path, home_dir: Path
) -> float:
    """Run hyperfine for `runs` iterations and return median ms.

    Uses `--prepare` to wipe the cache before every iteration so each
    measurement is a true cold start.
    """
    assert shutil.which("hyperfine") is not None
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", delete=False
    ) as result_file:
        result_path = result_file.name

    prepare_cmd = (
        f"rm -rf {shlex.quote(str(cache_dir))} && "
        f"mkdir -p {shlex.quote(str(cache_dir))}"
    )
    env = _isolated_env(cache_dir, home_dir)
    try:
        subprocess.run(
            [
                "hyperfine",
                "--warmup",
                "1",
                "--runs",
                str(runs),
                "--prepare",
                prepare_cmd,
                "--export-json",
                result_path,
                "--shell",
                "none",
                "--",
                *invocation,
            ],
            env=env,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        with open(result_path, "r", encoding="utf-8") as fh:
            payload = json.load(fh)
        results = payload.get("results", [])
        if not results:
            raise RuntimeError("hyperfine produced no results")
        result = results[0]
        # `median` is present in modern hyperfine; fall back to mean.
        seconds = result.get("median")
        if seconds is None:
            times = result.get("times") or []
            if not times:
                raise RuntimeError("hyperfine result lacks both median and times")
            sorted_times = sorted(times)
            seconds = sorted_times[len(sorted_times) // 2]
        return float(seconds) * 1000.0
    finally:
        try:
            os.unlink(result_path)
        except FileNotFoundError:
            pass


def _measure_with_perf_counter(
    invocation: list[str], runs: int, cache_dir: Path, home_dir: Path
) -> float:
    """Fallback path: subprocess + perf_counter, returns median ms."""
    samples: list[float] = []
    for _ in range(runs):
        if cache_dir.exists():
            shutil.rmtree(cache_dir)
        cache_dir.mkdir(parents=True, exist_ok=True)
        env = _isolated_env(cache_dir, home_dir)
        start = time.perf_counter()
        result = subprocess.run(
            invocation,
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        elapsed_ms = (time.perf_counter() - start) * 1000.0
        if result.returncode != 0:
            sys.stderr.write(result.stderr.decode("utf-8", errors="replace"))
            raise RuntimeError(
                f"invocation failed (exit {result.returncode}): {invocation}"
            )
        samples.append(elapsed_ms)
    samples.sort()
    return samples[len(samples) // 2]


def measure_cold_start(cmd: dict, tmp_root: Path) -> float:
    args = _build_invocation(cmd["args"], tmp_root)  # type: ignore[arg-type]
    cache_dir = tmp_root / "harn_cache"
    home_dir = tmp_root / "fake_home"
    home_dir.mkdir(parents=True, exist_ok=True)

    invocation = [HARN_BIN, *args]
    if shutil.which("hyperfine") is not None:
        return _measure_with_hyperfine(invocation, ITERATIONS, cache_dir, home_dir)
    return _measure_with_perf_counter(invocation, ITERATIONS, cache_dir, home_dir)


# --- Reporting ------------------------------------------------------------


def harn_version() -> str:
    try:
        out = subprocess.run(
            [HARN_BIN, "--version"],
            capture_output=True,
            text=True,
            check=True,
        )
        return out.stdout.strip()
    except (FileNotFoundError, subprocess.CalledProcessError):
        return "unknown"


def host_label() -> str:
    return f"{platform.system()} {platform.machine()}"


def main() -> int:
    budgets = load_budgets()
    baseline = load_baseline()
    sha = current_commit_sha()

    commands_to_run = TRACKED_COMMANDS
    if COMMANDS_FILTER:
        filter_set = set(COMMANDS_FILTER)
        commands_to_run = [c for c in TRACKED_COMMANDS if c["key"] in filter_set]
        missing = filter_set - {c["key"] for c in commands_to_run}  # type: ignore[arg-type]
        if missing:
            print(
                f"error: unknown bench keys in --commands filter: {sorted(missing)}",
                file=sys.stderr,
            )
            return 2

    using_hyperfine = shutil.which("hyperfine") is not None
    timer_label = "hyperfine" if using_hyperfine else "perf_counter fallback"
    print(
        f"# CLI cold-start benchmark (iterations={ITERATIONS}, timer={timer_label})"
    )
    print(f"# harn: {harn_version()}  host: {host_label()}  commit: {sha}")
    print(
        f"{'bench':<22} {'cold_ms':>10} {'budget':>10} {'baseline':>10} {'delta':>10} {'status':>10}"
    )

    measurements: dict[str, dict[str, float]] = {}
    failures: list[str] = []

    with tempfile.TemporaryDirectory(prefix="harn-cli-coldstart-") as raw:
        tmp_root = Path(raw)
        for cmd in commands_to_run:
            key = cmd["key"]  # type: ignore[assignment]
            per_cmd_root = tmp_root / key.replace(" ", "_").replace("/", "_")
            per_cmd_root.mkdir(parents=True, exist_ok=True)

            cold_ms = measure_cold_start(cmd, per_cmd_root)
            measurements[key] = {"cold_ms": round(cold_ms, 3)}

            budget = budgets[key]
            budget_ms = budget["cold_ms"]
            ratio = budget["regression_x"]

            baseline_sha, baseline_ms = most_recent_baseline_for(baseline, key, sha)

            failed_reasons: list[str] = []
            if cold_ms > budget_ms:
                failed_reasons.append(f"exceeds budget {budget_ms:.1f} ms")
            if baseline_ms is not None and cold_ms > baseline_ms * ratio:
                failed_reasons.append(
                    f"exceeds {ratio:.2f}x baseline ({baseline_ms:.1f} ms from {baseline_sha[:8] if baseline_sha else '-'})"
                )

            if baseline_ms is None:
                delta = "-"
                baseline_str = "-"
            else:
                pct = ((cold_ms - baseline_ms) / baseline_ms) * 100.0
                delta = f"{pct:+.1f}%"
                baseline_str = f"{baseline_ms:.1f}"

            status = "FAIL" if failed_reasons else "ok"
            if failed_reasons:
                failures.append(f"{key}: {'; '.join(failed_reasons)}")

            print(
                f"{key:<22} {cold_ms:>10.1f} {budget_ms:>10.1f} {baseline_str:>10} {delta:>10} {status:>10}"
            )

    if failures:
        # Do not poison the baseline ledger with a regressed run — the
        # next CI invocation should still compare against the last
        # known-good baseline, not the regression.
        print()
        print("FAIL: one or more subcommands regressed:")
        for line in failures:
            print(f"  - {line}")
        return 1

    # Passing run: update the baseline file. Two rules:
    #   - The current commit's slot is filled only if it was empty
    #     (append-only ledger), unless --update-baseline was passed.
    #   - Only commands measured in this run are written; commands not
    #     run in this invocation keep whatever was already recorded for
    #     this commit.
    existing_entry = baseline.get(sha, {})
    existing_commands = existing_entry.get("commands", {})
    merged_commands = dict(existing_commands)
    for key, payload in measurements.items():
        if key in merged_commands and not UPDATE_BASELINE:
            continue
        merged_commands[key] = payload

    baseline[sha] = {
        "host": existing_entry.get("host", host_label()),
        "harn_version": existing_entry.get("harn_version", harn_version()),
        "captured_at": _dt.datetime.now(_dt.timezone.utc).isoformat(
            timespec="seconds"
        ),
        "commands": merged_commands,
    }
    save_baseline(baseline)

    return 0


if __name__ == "__main__":
    sys.exit(main())
