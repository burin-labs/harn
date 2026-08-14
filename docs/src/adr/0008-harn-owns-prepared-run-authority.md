# ADR 0008: Harn owns prepared-run authority

## Status

Accepted on 2026-08-14 for
[#6662](https://github.com/burin-labs/harn/issues/6662), and extended by
[#6666](https://github.com/burin-labs/harn/issues/6666) and
[#6667](https://github.com/burin-labs/harn/issues/6667).

## Context

Harn and its hosts already enforce capabilities at several strong seams:
`CapabilityPolicy` attenuates workflow authority, the canonical permission and
network evaluators classify concrete operations, session grants preserve
delegation boundaries, and secret providers keep durable values out of Harn
source. These mechanisms currently meet only after a run has begun. A host can
therefore stage credentials, call a GUI keyring, install a process sandbox, or
attempt network I/O before discovering that another seam rejects the run.

Host-specific preflights have not solved this. They normalize the same request
differently from dispatch, cannot prove the runtime binary matches the contract
they inspected, and leave approval history spread across adapters. The failure
then appears as an endpoint, credential, or subprocess failure even when policy
was the actual decider.

## Decision

Harn owns one `PreparedRun` deep module. Its external interface is:

```text
prepare(intent, host_facts)
  -> Ready(authority_lease, receipt)
   | NeedsApproval(batched_requests)
   | Blocked(actionable_diagnostics)

execute(authority_lease)
request_delta(authority_lease, requirement)
```

`RunAuthorityPlan.v1` is the normalized, value-free contract. It combines the
compiled `CapabilityPolicy` with exact network destinations, secret references
and consumer bindings, admitted environment names, socket and MCP needs,
budgets, interactivity, runtime provenance, a startup deadline, and a receipt
location. Secret values are never fields in this contract.

Preparation persists a startup receipt before evaluating credentials or
performing any side effect. It intersects the requested capability policy with
the host ceiling, checks exact provenance and host facts, and sends each
requirement through the same canonical permission evaluator used by execution.
Network requirements additionally use the canonical `NetPolicy` evaluator.
Reviewable requirements are grouped by semantic authority family and
fingerprinted as one approval batch.

A successful decision creates an opaque, time-bounded `AuthorityLease` bound
to the plan, exact normalized requirements, canonical policies, and deciders.
Execution re-evaluates those same policies immediately before each declared
operation. Executors cannot extract serializable authority; they receive an
`AuthorityUse` interface and must authorize the exact requirement before its
effect. Terminal receipts record requested, granted, used, denied, and unused
authority plus the decider and policy evidence, without secret material.

Dynamic needs use a typed `AuthorityLeaseDelta`. A delta is bound to its parent
lease and may only attenuate an existing requirement. Widening requires a new
prepared run so a delta cannot silently turn an approved envelope into a larger
one.

Command-derived toolchain roots use a post-readiness discovery phase on the
same lease. The plan reviews an exact probe command and root ceiling. The
startup and readiness receipts exist before the probe runs, and every observed
operation becomes a typed delta. Accepted roots attenuate the reviewed ceiling;
any widening invalidates the lease and requires a new grouped decision.

Platform identity is distinct from secret lookup. A consumer-bound broker
advertises its interaction, GUI, sandbox, renewal, source, and exact binding
facts. The plan fingerprints a value-free identity reference with the broker,
provider, audience, tenant, and consumer. Broker implementations return only a
non-serializable process-local handle, and durable material remains with the
host owner. Burin and Harn Cloud implement this shared Harn interface rather
than maintaining provider-chain fallbacks.

Burin owns approval presentation, permission persistence, native secret
brokers, and headless JSON/NDJSON projection. Harn Cloud owns hosted brokers and
tenant policy. Both adapt those facts to `PreparedRun`; neither reimplements
plan normalization or policy matching.

## Falsifiers

This decision must be revisited if:

1. preparation and dispatch require different policy semantics for the same
   normalized requirement;
2. a host must expose a secret value in `RunIntent`, a lease, or a receipt;
3. a serializable lease can authorize an operation after process restart;
4. a useful dynamic requirement cannot be expressed as attenuation or a new
   prepared run;
5. a canonical run can perform credential resolution, model spend, subprocess
   launch, network I/O, or sandbox installation before its startup receipt; or
6. Burin or Cloud must maintain a second authority taxonomy to present or
   broker the contract.

## Consequences

Preparation becomes a mandatory launch phase for adopted product paths. A
stale runtime, unavailable non-interactive broker, denied network destination,
or unavailable approval blocks before side effects with an attributable
diagnostic. Routine operations inside the lease do not prompt again.

Hosts must supply truthful facts and durable receipt sinks. The first version
does not itself implement a GUI, a durable secret store, or a remote broker; it
defines the narrow seam those owners implement. Existing host preflights and
launch-time raw environment forwarding should be removed as each canonical path
cuts over, rather than retained as parallel policy systems.

## Rejected alternatives

- **Burin-only launch preflight.** This leaves Harn Cloud and other embedders
  without the same contract and preserves evaluator drift at dispatch.
- **Serializable bearer grants.** A copied receipt or plan must never become
  authority; leases remain process-local opaque values.
- **Resolve secrets, then prepare.** This violates the startup ordering and can
  trigger GUI-capable keyrings or expose values for runs that policy later
  rejects.
- **Approve each tool call.** This produces prompt storms and cannot review the
  complete run envelope or account for unused grants.
- **Free-form authority maps.** Typed requirement variants are required so
  attenuation, grouping, schema validation, and audit evidence remain
  mechanically checked.
