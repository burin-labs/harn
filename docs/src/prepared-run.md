# Prepared-run authority

`harn_vm::prepared_run` reconciles everything a workflow needs with everything
its host can safely provide before execution starts. It is intended for product
hosts, headless runners, and hosted schedulers that embed `harn-vm`.

The host constructs a value-free `RunIntent` from the workflow's compiled
`CapabilityPolicy` and its additional network, secret-consumer, environment,
socket, MCP, budget, provenance, and startup requirements. `HostFacts` contains
observed ceilings and the canonical permission and network evaluators. Its
permission evaluator is a `RunApprovalPolicy`, constructed from one typed
`RunAuthorityPosture`: run interactivity, approval availability, and workspace
trust. Hosts use `WorkspaceTrust::HostMaterialized` for isolated workspaces they
created for CI, eval, scheduled, or hosted execution; this is a run fact, not a
durable per-path trust-store entry. `permits_project_policy` returns false for
`WorkspaceTrust::Untrusted` and true for `Trusted` and `HostMaterialized`.
Calling `prepare` yields one of three outcomes:

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

`PreparedRunExecutor` has separate `Output` and `Error` associated types.
`ExecutionOutcome::ExecutorFailed` returns the concrete executor error beside
the terminal authority receipt, so hosts can retain structured partial-run
evidence without parsing a rendered string or rereading a shared event store.
`ExecutionOutcome::AuthorityFailed` is distinct: it reports lease validation
or receipt persistence failures owned by Harn. Existing string-only executors
use `type Error = String`; no adapter or side channel is required.

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

When a run is both non-interactive and unable to obtain approval,
`RunApprovalPolicy::construct` resolves every rule, legacy pattern, and repeat
guard whose disposition is `ask` to a deterministic denial. The resulting
policy can therefore never defer to a human who does not exist. The host's
construction callback receives the same posture and selects its workspace
policy layers from `workspace_trust`; it must not infer this fact from a path
layout.

`discover_toolchain` is the only command-derived root path. Preparation reviews
the exact command and a read-root ceiling, then persists readiness. Only after
that receipt exists may a host `ToolchainProbeRunner` spawn the command. Harn
turns every observed root or attempted operation into a typed lease delta. A
narrower process-read root is applied atomically and remains unused until
dispatch authorizes it. A wider path, network access, secret access, process
write, or budget change is refused and receipted without invalidating the
already reviewed parent lease. Interactive hosts receive one new grouped
decision and non-interactive hosts receive an actionable block; in either case,
the caller decides whether the unavailable discovery was optional or a startup
precondition. Missing toolchains, malformed probe output, and probe policy
denials likewise leave the parent lease executable. Integrity failures still
invalidate it: an expired lease, a probe outside the fingerprinted lease, or a
discovery receipt that cannot be persisted.

`PreparedSession` is the versioned, replay-safe host protocol around this
engine. `prepare` emits `needs_approval`, `ready`, or `blocked`; one persisted
approval decision converts the grouped request into a fingerprinted session
lease. `attach` binds that lease to the exact session, workspace, runtime
provenance, and consumer before engine startup. A durable
`PreparedSessionLeaseStore` atomically rejects replay across server processes.
Routine turns reuse the attached authority envelope without prompting again,
and `stop`, `pivot`, and `terminal` all persist terminal accounting. The JSON
Schema is `schemas/prepared-session-v1.schema.json`; generated Rust,
TypeScript, Swift, Python, and Go protocol artifacts expose the same states and
commands.

`PreparedRun::request_delta` remains the typed attenuation interface for a
single prepared run. `PreparedSession::request_delta` adds the interactive
session behavior: an identical requirement is already covered, attenuation is
immediate, and widening produces one fingerprinted semantic approval batch.
Only the exact approved widening joins the active envelope; a denial leaves
the existing session usable.

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
custody of their own durable stores. At provider dispatch Harn re-reads broker
facts, acquires the handle, and marks identity used only after the exact
consumer successfully opens it. Broker-managed identities get one
reacquisition when a handle expires. Bedrock and Vertex cannot fall back to
ambient SDK, profile, metadata, environment, or credential-file chains inside
a prepared session; calls outside one retain their compatibility resolvers.

Every terminal receipt records requested, granted, used, denied, and unused
authority, along with its decider and canonical policy-decision evidence. A
receipt never grants authority and must not contain secret material.
