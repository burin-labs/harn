# Fleet coordination

`std/fleet/coordination` adds a typed fleet vocabulary and a deterministic
status projection on top of Harn's existing durable coordination ledger. It is
an explicit-import library: importing it registers no trigger, timer, reminder,
supervisor, or reassignment behavior.

Use the lower-level [`std/coordination`](./agent-channels.md#coordination-ledger)
helpers for general agent messages. Use this module when a host or workflow
needs stable records for work ownership, acknowledgements, premise-changing
findings, liveness, completion receipts, or structured check-ins.

## Existing substrate

The module does not create another mailbox or claim engine:

- agent channels own durable append-only storage, cursors, idempotency, signed
  emit timestamps, and replay receipts;
- `std/coordination` owns scoped rooms, actor/ref metadata, addressing, replies,
  and consumer acknowledgements;
- `worker://` queues remain authoritative for queue-dispatched work claims;
- persona leases remain authoritative for persona work.

A fleet claim can reference a queue claim or persona lease. It does not replace
either state machine.

## Posting records

```harn
import {
  fleet_ack,
  fleet_claim,
  fleet_finding,
  fleet_heartbeat,
} from "std/fleet/coordination"

const claim = fleet_claim("workspace", "release", "repo:release", {
  id: "release-claim-17",
  workspace_id: "sdk-workspace",
  from: {agent: "release-worker"},
  requires_ack: true,
  lease_ms: 300000,
  refs: [{kind: "pull_request", url: "https://example.invalid/pr/17"}],
})

fleet_ack("workspace", "release", claim.id, "accepted", {
  id: "release-claim-17-ack",
  workspace_id: "sdk-workspace",
  from: {agent: "controller"},
})

fleet_heartbeat("workspace", "release", "busy", {
  id: "release-worker-heartbeat-1",
  workspace_id: "sdk-workspace",
  from: {agent: "release-worker"},
  current_claim_ids: [claim.id],
  lease_ms: 300000,
  expected_silence_ms: 600000,
})

fleet_finding("workspace", "release", "overturned", {
  id: "release-finding-1",
  workspace_id: "sdk-workspace",
  from: {agent: "release-worker"},
  subject: "The target artifact is generated elsewhere",
  refs: [{kind: "source", ref: "generator/README.md"}],
})
```

The public helpers are:

| Helper | Coordination kind | Data schema |
|---|---|---|
| `fleet_claim` | `claim` | `harn.fleet.claim.v1` |
| `fleet_ack` | `ack` | `harn.fleet.ack.v1` |
| `fleet_done` | `done` | `harn.fleet.done.v1` |
| `fleet_finding` | `finding` | `harn.fleet.finding.v1` |
| `fleet_heartbeat` | `heartbeat` | `harn.fleet.heartbeat.v1` |
| `fleet_checkin` | `checkin` | `harn.fleet.checkin.v1` |

Every helper returns the normal `harn.coordination.receipt.v1` value. References
stay on the coordination envelope so hosts can render them without parsing body
text.

### Runtime-owned time

The coordination write path rejects caller-supplied `created_at`, `ts`, and
`seq`; fleet helpers enforce the same boundary before delegating. Event order
and liveness use the channel event's signed `emitted_at.at_ms`, not a caller
timestamp. A source timestamp imported from another system is metadata (for
example, `data.claimed_ts`) and must not become the authoritative fleet clock.

Actor fields are attribution, not cryptographic identity. Hosts must not treat
an `agent` string alone as authentication.

## Projecting a snapshot

Read coordination rows with their channel event wrappers, then inject the clock
used for the projection:

```harn
import { coord_read } from "std/coordination"
import { fleet_project } from "std/fleet/coordination"

const rows = coord_read("workspace", "release", {
  workspace_id: "sdk-workspace",
  include_events: true,
})

const snapshot = fleet_project(rows, {
  now_ms: harness.clock.timestamp() * 1000,
  grace_ms: 60000,
})
```

`fleet_project` returns `harn.fleet.snapshot.v1` with:

- actors and `active`, `late`, `unresponsive`, or `unknown` liveness;
- claims in `proposed`, `active`, `completed`, `failed`, `abandoned`,
  `rejected`, `needs_changes`, `stale`, or `conflicted` state;
- conflicts when more than one live claim names the same work key;
- premise-changing findings still waiting for an acknowledgement;
- the latest structured check-in for each actor;
- generic summaries of messages whose fleet data schema is newer or unknown.

An expected-silence window extends the heartbeat deadline and any current claim
ids named by that heartbeat. This lets a worker declare a long non-chatty build
without being mistaken for an unresponsive lane.

The projector never chooses a winning claim, acknowledges a finding, releases
a resource, or reassigns work. Those are controller decisions and require
separate explicit receipts. A completion receipt changes claim state only when
both its claim id and work key match the original claim.

## Queue and persona claims

Use `queue_claim_id` or `persona_lease_id` on `fleet_claim` to link an operator
view to the authoritative runtime receipt. Do not mirror claim renewal with
fleet messages when WorkerQueue or persona runtime already owns it. Manual or
externally executed work can use the fleet claim lease directly.

## Reserving a host

Fleet claims describe work ownership. They do not reserve CPU, GPU, or another
exclusive machine resource. Harn workflows should use the cancellation-safe
`std/host_lease::with_host_lease` scope when independent processes must
serialize multi-step work on one host:

```harn
import { with_host_lease } from "std/host_lease"

const result = with_host_lease(
  {
    owner: "release-audit",
    resource_class: "rust-heavy",
    domain: "release-audit",
    priority_class: "ci-verify",
    wait_timeout_ms: 20 * 60 * 1000,
  },
  { _handle -> run_release_audit() },
)
```

The VM atomically adopts an opaque cleanup guard with every successful
acquisition. Normal returns and exceptions write an explicit typed release
receipt; forced task cancellation, frame teardown, and VM drop invoke the same
idempotent cleanup through guard lifetime. Acquisition waits in bounded slices
on registry notifications rather than sleeping or parsing process tables.

When deterministic discovery finishes after acquisition, replace the active
lease metadata without introducing a sidecar authority or releasing ownership:

```harn
import { host_lease_update_metadata, with_host_lease } from "std/host_lease"

with_host_lease(
  {
    owner: "indexer",
    domain: "repository-index",
    metadata: {phase: "discovering"},
  },
  { handle ->
    const update = host_lease_update_metadata(
      handle,
      {phase: "ready", revision: discover_revision()},
    )
    build_index(update.handle)
  },
)
```

Metadata updates require the active token and exact resource domain. They
replace the complete map atomically, so omitted keys are removed and retries do
not depend on prior merge state. The scope retains its opaque cleanup guard.

Use the `harn host lease` CLI adapter for non-Harn commands:

```bash
harn host lease acquire \
    --host build-01 \
    --resource-class rust-heavy \
    --domain release-audit \
    --owner eval-runner \
    --priority-class measurement \
    --no-expiry \
    --owner-pid "$runner_pid" \
    --json
```

Acquisition is an atomic cross-process operation. A successful receipt carries
the `lease_id` required by `renew` and `release`. A contended immediate acquire
returns a typed `host_lease_contended` receipt and exit status 75. Add
`--wait-ms <milliseconds>` to wait on lease-change notifications, the current
lease's expiry, or a bounded owner-liveness recheck instead of a fixed polling
loop.

Waiting acquisitions join one durable queue per machine, resource class, and
domain. Higher priority classes are admitted first; requests within one class
are FIFO by original request time with a stable waiter-id tie break. Each
acquisition receipt includes `queue` evidence with the waiter id, original
request time, one-based position, and immediate predecessor. A supervised run
reuses its run id and original request time after a worker restart, so a later
process cannot steal its place.

A contended wait should stay near idle until one of those events occurs. Harn
uses a dedicated lease-change signal for this path; routine SQLite registry
traffic does not wake the waiter. If an older Harn process uses substantial CPU
while it only waits for a host lease, upgrade Harn before starting more queued
work.

Finite leases default to 15 minutes and should be renewed by long-running work.
`--no-expiry` requires the PID of the process that owns the work. Harn records
that process's start identity and recovers the lease after a confirmed crash
without mistaking unavailable process visibility for death. This is the
appropriate lifetime for an authoritative measurement that must not expire
mid-run.

`whole-machine` remains the default resource class for existing callers.
`rust-heavy` is a separate capacity-one class for CPU-, linker-, and
cache-intensive Cargo work, so independent Rust verification can serialize
without conflating it with every host-level activity. A domain names an
independent capacity-one lease within a resource class. Use stable, semantic
domains when one host must serialize several unrelated workflows. The default
domain preserves the original single-lease behavior. Inspect and release the
same resource and domain explicitly:

```bash
harn host lease status \
    --host build-01 \
    --resource-class rust-heavy \
    --domain release-audit \
    --json
harn host lease release \
    --host build-01 \
    --resource-class rust-heavy \
    --domain release-audit \
    --lease-id "$lease_id" \
    --json
```

When an acquire or status operation transactionally removes an expired or
dead-owner lease, its receipt includes the exact prior handle in `recovered`.
Callers can make deterministic recovery decisions from its typed identity and
metadata; the compatibility boolean `recovered_stale_lease` is derived from
the same evidence.

For Cargo work, use the supervised boundary instead of manually acquiring a
lease around a launcher process:

```bash
harn host lease run cargo \
    --host build-01 \
    --owner ci-verify \
    --priority-class ci-verify \
    --wait-ms 3600000 \
    --workload-timeout-ms 600000 \
    --workspace "$workspace" \
    --target-dir "$CARGO_TARGET_DIR" \
    --build-dir "$CARGO_BUILD_BUILD_DIR" \
    -- test -p harn-vm
```

The worker acquires `rust-heavy` before Cargo starts. `--wait-ms` bounds only
queue admission; `--workload-timeout-ms` starts after acquisition and bounds
only the Cargo process tree. On Windows the worker also enrolls itself in a
kill-on-close Job Object. On normal completion the supervisor writes a
redacted receipt under
`~/.harn/host-leases/receipts/` containing wait time, hold time, exit status,
resource identity, queue evidence, and hashed workspace/target/build
identities.

The worktree Cargo wrapper uses this boundary by default for commands that may
compile or mutate build artifacts: build, check, clean, clippy, doc, fix,
install, nextest, package, publish, run, rustc, rustdoc, test, bench, and
unknown Cargo plugins. Static commands such as fmt, metadata, tree, fetch, and
update bypass the lease. Artifact isolation remains unchanged.

The wrapper first looks for an already-built Harn binary in the active target
directory, then on `PATH`. It never builds a lease runner recursively. A first
build with no compatible runner prints one warning and runs Cargo directly;
subsequent commands use the binary that build produced. CI defaults this local
admission layer off because workflow jobs own their runner concurrency.

Automatic runs wait up to one hour on lease-change notifications and use the
interactive priority class. Override those defaults or require an exact runner
when a script needs a fail-closed boundary:

```bash
HARN_CARGO_LEASE_RUNNER=/path/to/prebuilt/harn \
HARN_CARGO_LEASE_OWNER=ci-verify \
HARN_CARGO_LEASE_PRIORITY_CLASS=ci-verify \
HARN_CARGO_LEASE_WAIT_MS=600000 \
HARN_CARGO_LEASE_WORKLOAD_TIMEOUT_MS=600000 \
HARN_CARGO_LEASE_MODE=required \
./scripts/cargo_with_worktree_build_dir.sh test -p harn-vm
```

`HARN_CARGO_LEASE_MODE` accepts `auto` (the local default), `required`, or
`off`. Use `off` only for an intentionally independent lane:

```bash
HARN_CARGO_LEASE_MODE=off make vm-check
```

The wrapper consumes its request variables before launching Harn and marks the
supervised Cargo process tree internally. Build scripts, tests, and nested Make
calls therefore cannot inherit the request or wait behind their own
capacity-one lease.

State defaults to `~/.harn/host-leases`. Set `HARN_HOST_LEASE_ROOT` to isolate
tests or relocate state for processes running under the same OS account. A
cross-user registry requires a privileged host service and is not provided by
this local capability. The host-lease store owns priority and FIFO admission
for native resources. Harn's worker scheduler separately owns orchestration
queue policy, including low-priority promotion after a bounded 30-minute wait;
the two queues coordinate different resources and expose separate typed
receipts.

## Forward compatibility

The coordination envelope allows additional properties. A projector that sees
an unknown fleet data schema includes a generic entry in `unknown_messages`
instead of treating it as a known lifecycle transition. This keeps newer
producers visible to older read surfaces without letting an unknown record
change claim or liveness state.
