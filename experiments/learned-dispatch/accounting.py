"""Read the native provider ledger without turning unknown charges into zero."""
from dataclasses import dataclass
import math


@dataclass(frozen=True)
class Accounting:
    known_cost_usd: float
    cost_usd: float | None
    physical_calls: int
    unknown_calls: int
    unpriced_calls: int


def nonnegative_cost(value: object) -> float:
    assert type(value) in (int, float) and math.isfinite(value) and value >= 0, 'invalid cost'
    return float(value)


def measure(calls: list[dict], usage: dict, *, allow_unknown: bool = False) -> Accounting:
    assert calls and len({call['call_id'] for call in calls}) == len(calls), 'missing or duplicate call audit'
    assert usage['total_cost_coverage'] == 'live_provider_ledger', 'provider ledger is unavailable'
    for call in calls:
        for key in ('provider_call_count', 'usage_unknown_calls', 'unpriced_calls'):
            assert type(call[key]) is int and call[key] >= 0, f'invalid {key}'
        assert call['provider_call_count'] > 0, 'unmeasured physical call'
        assert call['usage_unknown_calls'] <= call['provider_call_count']
        assert call['unpriced_calls'] <= call['provider_call_count']
        known = nonnegative_cost(call['known_cost_usd'])
        incomplete = call['usage_unknown_calls'] > 0 or call['unpriced_calls'] > 0
        assert (call['cost_usd'] is None) == incomplete, 'contradictory cost certainty'
        if incomplete:
            assert allow_unknown, 'incomplete provider accounting'
            assert call['call_role'] != 'decision', 'decision accounting is incomplete'
        else:
            assert math.isclose(nonnegative_cost(call['cost_usd']), known, abs_tol=1e-10)
    physical = sum(call['provider_call_count'] for call in calls)
    known = sum(call['known_cost_usd'] for call in calls)
    unknown = sum(call['usage_unknown_calls'] for call in calls)
    unpriced = sum(call['unpriced_calls'] for call in calls)
    assert type(usage['total_provider_calls']) is int and usage['total_provider_calls'] == physical, 'physical-call census differs'
    assert math.isclose(nonnegative_cost(usage['total_known_cost_usd']), known, abs_tol=1e-10), 'known-cost census differs'
    cost = None if unknown or unpriced else known
    assert (usage['total_cost_usd'] is None) == (cost is None), 'rollup cost certainty differs'
    if cost is not None:
        assert math.isclose(nonnegative_cost(usage['total_cost_usd']), cost, abs_tol=1e-10), 'settled-cost census differs'
    return Accounting(known, cost, physical, unknown, unpriced)
