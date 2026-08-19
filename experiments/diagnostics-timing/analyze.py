#!/usr/bin/env python3
"""Aggregate diagnostics-timing trial rows into a decision table.

    ./analyze.py runs/stage1.jsonl
    ./analyze.py runs/stage1.jsonl --control A-push-all

Reports, per arm: pass rate, turns to green, and the diagnostic-exposure
counters, plus a paired 95% bootstrap interval against the control arm on the
decision metric. Pairing is by (fixture, trial), which holds the fixture and the
trial index fixed so the arm is the only thing that differs.

A run that did not pass has no turns-to-green, so turns are reported over passing
trials only and the pass rate is reported beside them. An arm that converges fast
by giving up is not a winner, and separating the two numbers is what makes that
visible.
"""

from __future__ import annotations

import argparse
import json
import random
import statistics
from collections import defaultdict


def load(path):
    rows = []
    with open(path) as handle:
        for line in handle:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def bootstrap_ci(deltas, iterations=10000, seed=17):
    """Paired bootstrap interval over per-pair deltas."""
    if len(deltas) < 2:
        return (float("nan"), float("nan"))
    rng = random.Random(seed)
    means = []
    size = len(deltas)
    for _ in range(iterations):
        sample = [deltas[rng.randrange(size)] for _ in range(size)]
        means.append(sum(sample) / size)
    means.sort()
    lo = means[int(0.025 * iterations)]
    hi = means[int(0.975 * iterations)]
    return (lo, hi)


def mean(values):
    return statistics.mean(values) if values else float("nan")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("path")
    parser.add_argument("--control", default="A-push-all")
    parser.add_argument("--metric", default="iterations")
    args = parser.parse_args()

    rows = load(args.path)
    if not rows:
        print("no rows")
        return

    by_arm = defaultdict(list)
    for row in rows:
        by_arm[row["arm"]].append(row)

    fixtures = sorted({row["fixture"] for row in rows})
    print(f"fixtures: {', '.join(fixtures)}")
    print(f"trials:   {len(rows)}  arms: {len(by_arm)}")
    print()

    header = (
        f"{'arm':16} {'n':>3} {'pass':>6} {'turns*':>7} {'tools':>6} "
        f"{'tok':>8} {'wall_s':>7} {'shown':>6} {'trans':>6} {'noise':>6} {'detour':>7}"
    )
    print(header)
    print("-" * len(header))

    for arm in sorted(by_arm):
        trials = by_arm[arm]
        passed = [t for t in trials if t.get("passed")]
        print(
            f"{arm:16} {len(trials):>3} "
            f"{len(passed) / len(trials):>6.2f} "
            f"{mean([t['iterations'] for t in passed]):>7.1f} "
            f"{mean([t['tool_calls'] for t in trials]):>6.1f} "
            f"{mean([t['input_tokens'] + t['output_tokens'] for t in trials]):>8.0f} "
            f"{mean([t['elapsed_ms'] for t in trials]) / 1000:>7.1f} "
            f"{mean([t['flush_count'] for t in trials]):>6.2f} "
            f"{mean([t['surfaced_transient'] for t in trials]):>6.2f} "
            f"{mean([t['surfaced_noise'] for t in trials]):>6.2f} "
            f"{mean([t['detour_calls'] for t in trials]):>7.2f}"
        )

    print()
    print("* turns over PASSING trials only; read it beside the pass rate.")
    print()

    if args.control not in by_arm:
        print(f"control arm {args.control!r} absent; skipping paired intervals")
        return

    def keyed(arm):
        out = {}
        for row in by_arm[arm]:
            out[(row["fixture"], row["trial"])] = row
        return out

    control = keyed(args.control)
    print(f"paired 95% bootstrap vs {args.control} (negative favours the arm)")
    print(f"{'arm':16} {'pairs':>6} {'d_' + args.metric:>12} {'95% CI':>22} {'d_pass':>8}")
    print("-" * 68)

    for arm in sorted(by_arm):
        if arm == args.control:
            continue
        other = keyed(arm)
        shared = sorted(set(control) & set(other))
        if not shared:
            continue
        metric_deltas = []
        pass_deltas = []
        for key in shared:
            a, b = control[key], other[key]
            pass_deltas.append(int(bool(b.get("passed"))) - int(bool(a.get("passed"))))
            if a.get("passed") and b.get("passed"):
                metric_deltas.append(b[args.metric] - a[args.metric])
        lo, hi = bootstrap_ci(metric_deltas)
        delta = mean(metric_deltas)
        verdict = ""
        if lo == lo and hi == hi:
            if hi < 0:
                verdict = "  <- better"
            elif lo > 0:
                verdict = "  <- worse"
        print(
            f"{arm:16} {len(shared):>6} {delta:>12.2f} "
            f"{f'[{lo:+.2f}, {hi:+.2f}]':>22} {mean(pass_deltas):>8.2f}{verdict}"
        )

    print()
    print("An interval spanning zero is no separation. Say so rather than ranking means.")


if __name__ == "__main__":
    main()
