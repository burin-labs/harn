# Ambient host conditions

`std/host_conditions` exposes a read-only snapshot of ambient contention:

```harn
import { host_conditions_sample } from "std/host_conditions"

const snapshot = host_conditions_sample()
for answer in snapshot.questions {
  harness.stdio.log(answer.question, answer.status, answer.contention)
}
```

The contract names questions rather than platform signals:

| Question | Meaning |
| --- | --- |
| `promised_cpu` | Is this workload receiving the CPU capacity it was promised? |
| `nominal_speed` | Is execution running below nominal speed? |
| `accelerator_shared` | Is the accelerator allocation shared? |
| `memory_or_io_contended` | Is memory or storage pressure delaying work? |

Each answer has exactly one observability state:

- `observed` includes `contention` from `0.0` (a real quiet reading) through
  `1.0` (maximum normalized contention).
- `unavailable` means the environment should expose the answer but this read
  failed. Retry or alert on repeated failures.
- `not_observable` means the environment cannot expose the answer. Do not
  retry forever or treat it as quiet. For example, a guest cannot observe host
  thermal throttling, so `nominal_speed` is `not_observable`, never an observed
  zero.

Callers threshold the question and do not branch on Linux, VM, container, or
signal names. The local backend uses load relative to available CPUs on
ordinary hosts, CPU steal in virtual guests, cgroup quota/throttling in
containers, CPU frequency where visible, and Linux memory/IO pressure
counters. Missing accelerator allocation metadata is reported honestly rather
than inferred.

Managed embedders can install `HostConditionsCapability::with_source(...)`
with a `HostConditionsSource`. The injected source returns the same
`HostConditionsSnapshot` type as local probing, so Harn callers keep the same
`host_conditions_sample()` call while a control plane supplies richer facts.
The capability validates the version, the complete four-question set, and
state/value invariants at that boundary.

Sampling reads a small, bounded set of in-memory kernel files and starts no
processes or network requests. `sample_cost_us` records the actual end-to-end
cost of every sample; the intended cost is microseconds to low milliseconds,
small enough to sample for every unit of measurement.

## Correctness comes from assignment

Randomized assignment plus blocking on host is the correctness guarantee for
comparisons. It works even when the environment exposes no contention facts.
Ambient conditions are only an optimization and attribution aid: they can
avoid wasting trials on a loud host and explain suspicious measurements.
They never substitute for randomized assignment. Without randomized
assignment, telemetry can make a biased comparison look controlled; without
telemetry, randomized assignment remains correct but may require more trials.
