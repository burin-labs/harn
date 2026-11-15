# Build target ownership

The target collector manages Cargo output beneath `harn-target` and
`release-gate-target`. `scripts/target_gc_maintenance.sh` fetches the current
collector from main before each scheduled or disk-pressure sweep.

## Checkout records

Setup records the checkout that owns its managed target after restoring any
dependency seed. The record is `.harn-target-owner.json` inside the target:

```json
{"schema":"harn.target-owner/v1","checkout":"/absolute/path/to/checkout"}
```

The checkout path is physical and absolute. The record is replaced atomically
and is excluded from dependency seeds. A seed therefore cannot transfer its
publisher's checkout ownership to another target.

An existing registered checkout makes its target a warm cache, including when
the checkout is a standalone clone outside the search roots. Missing checkouts
do not keep targets alive. Malformed or unreadable ownership records keep the
target and produce an incomplete sweep with a pending count.

Targets without a record retain discovery through Git worktree listings and
`.cargo/config.toml`. The default search roots are `~/projects`,
`~/remote-builds`, `~/.codex/worktrees` and `/private/tmp`.
`HARN_TARGET_GC_ROOTS` replaces that list. A valid target record takes precedence
over these legacy basename mappings.

The collector also exposes explicit registration without running a sweep:

```sh
scripts/prune_stale_targets.sh --record-owner "$TARGET" "$CHECKOUT"
```

`TARGET` must already be a directory immediately beneath a managed cache root.
`CHECKOUT` must be the root of an existing Git checkout. Normal `make setup`,
`make setup-rust` and `make setup-bootstrap` perform registration automatically
for their managed target.

## Liveness decisions

A live process holding the target or its owning checkout prevents removal.
The native census includes working directories and open handles. The same
census runs again immediately before a removal, so an earlier idle reading
does not authorize deleting a newly active target.

A Git checkout's `locked` record also prevents removal. Manual locks remain
active until removed. Version-one `burin-claim` records protect the target
until their declared expiry; an expired claim does not override a live process.
Unreadable or malformed claims keep the target and report pending ownership.

These protections apply before age, rank, disk-pressure and named-removal
rules. An idle registered checkout can still have its cache reclaimed under
those bounds. An unavailable process census retains targets and reports the
unmeasured state rather than treating it as an idle machine.
