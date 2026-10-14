# Smart Ship

Smart Ship combines reviewed changes into immutable verification snapshots and
publishes a release checkpoint only when every required job has valid evidence.
The package runs on ordinary Git projects. Repository names, build commands,
execution environments, verification keys, and budgets belong to installation
policy.

[Install and control Smart Ship](docs/install.md).

The package provides signed verification and checkpoint contracts, durable Git
controller state, bounded repair claims, and a standard Harn repair agent. Live
forge admission and fleet cutover are tracked in
[harn#9857](https://github.com/burin-labs/harn/issues/9857).

## Verify the package

```sh
harn package verify personas/smart_ship --strict
```

## Contract

Import `harn-smart-ship/lib` after installing the package. `verify` reports the
required job count and the names of pending, failed, infrastructure-failed, and
invalid jobs. Reused evidence retains the signed original run, attempt, job,
source, logs, and artifacts. Input, runtime, environment, coverage, and trust
identities must all match the candidate's requirement.

`promote` evaluates that evidence and the current controller fence. A stopped,
held, shadow, or stale controller receives a refusal. A draining controller can
finish already admitted verification while refusing new admission. Successful
assessment returns an immutable checkpoint; it does not mutate a branch or
deployment. The effect adapter must atomically compare the fence at its durable
owner before publication. A prior read of a lease is insufficient.

`checkpoint_valid` verifies both the original execution signatures and the
checkpoint publisher's signature. Consumers use `release_identity` to obtain
the exact proven manifest identity. A moving branch is a convenience projection.

Scope adapters own complete job selection and execution-input identities. When
affected-test coverage cannot be established, they must require the full suite.
A signature authenticates the executor's report; it does not prove that its
test inventory covers the application. Incomplete and empty inventories fail.

`offer`, `start`, and `complete` coalesce pending cumulative work while preserving
the running snapshot. `claim_repair` reserves one bounded incident owner.
`repair_authorized` checks a generation-bound permit against freshly read state
before a worker effect. Stop revokes claims, including after resume. Transfer
requires drained workers and moves authority to a new generation in shadow mode.
The Git state adapter commits transitions with remote compare-and-swap; forge
effects must enforce the same authority at their mutation owner.

The Python and TypeScript fixtures execute ordinary commands against clean Git
sources. Each reviewed change passes independently; their combination fails one
test. `fixtures/live_repair.harn` uses a real configured model, independently
verifies the repair, and checks the signed checkpoint through the consumer
interface. Fixture review receipts are synthetic; these local examples do not
qualify GitHub review admission or a production controller.
