# Prepared-run authority

`harn_vm::prepared_run` reconciles everything a workflow needs with everything
its host can safely provide before execution starts. It is intended for product
hosts, headless runners, and hosted schedulers that embed `harn-vm`.

The host constructs a value-free `RunIntent` from the workflow's compiled
`CapabilityPolicy` and its additional network, secret-consumer, environment,
socket, MCP, budget, provenance, and startup requirements. `HostFacts` contains
observed ceilings and the canonical permission and network evaluators. Calling
`prepare` yields one of three outcomes:

- `Ready` contains an opaque fingerprinted lease and a persisted readiness
  receipt.
- `NeedsApproval` contains one fingerprinted batch, grouped into reviewable
  authority families.
- `Blocked` contains actionable diagnostics, such as stale runtime provenance,
  unavailable approval, a secret-consumer mismatch, or a policy denial.

The first receipt event is persisted before host validation. A host must call
`prepare` before credential resolution, model calls, subprocesses, network I/O,
or process-sandbox installation. An executor passed to `PreparedRun` receives
only `AuthorityUse`; it calls `authorize` with the exact typed requirement
immediately before each material operation. Dispatch repeats the canonical
evaluation and rejects any requirement not fingerprinted into the lease.

The normative JSON Schema for the normalized plan is
[`run-authority-plan.v1.json`](../schemas/run-authority-plan.v1.json). The
serialized contract contains secret references and exact consumer bindings,
never secret values. `AuthorityLease` is deliberately not serializable;
receipts are evidence, not reusable authority.

Non-interactive hosts must advertise only secret brokers that both support
non-interactive access and cannot invoke GUI-capable keyring APIs. Environment
and dotenv credentials should be moved into a zeroizing process-local provider
and scrubbed from the process environment before workload execution. Durable
secrets remain behind a host broker outside the workload sandbox.

`discover_toolchain` is the only command-derived root path. Preparation reviews
the exact command and a read-root ceiling, then persists readiness. Only after
that receipt exists may a host `ToolchainProbeRunner` spawn the command. Harn
turns every observed root or attempted operation into a typed lease delta. A
narrower process-read root is applied atomically and remains unused until
dispatch authorizes it. A wider path, network access, secret access, process
write, or budget change invalidates the lease; interactive hosts receive one
new grouped decision and non-interactive hosts receive an actionable block.

`request_delta` remains the general typed attenuation interface. An identical
requirement is already covered. A narrower filesystem root produces a delta
bound to the parent lease and its expiry. A wider or unrelated requirement is
blocked and requires a newly prepared run.

SDK profiles, workload identity, instance metadata, and hosted credentials use
`ConsumerBoundIdentityBroker`. `RunAuthorityPlan.v1` records only a
`harn-identity://` reference plus exact broker, source, renewal, provider,
audience, tenant, and consumer facts. A host advertises whether the broker is
non-interactive, GUI-capable, host-isolated, renewing, and able to return
opaque process-local handles for that exact binding. Changing any binding fact
blocks readiness before broker acquisition or provider spend.

`OpaqueIdentityHandle` is non-cloneable and non-serializable. Its zeroizing
material can be consumed once through the complete fingerprinted requirement;
the handle rejects a different provider, audience, tenant, consumer, or broker.
Local and hosted adapters implement the same broker interface while retaining
custody of their own durable stores.

Every terminal receipt records requested, granted, used, denied, and unused
authority, along with its decider and canonical policy-decision evidence. A
receipt never grants authority and must not contain secret material.
