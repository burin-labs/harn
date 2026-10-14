# Install and control Smart Ship

Use a qualified Harn runtime that satisfies this package's `harn.toml` range.
The neutral examples also require Git, Python 3, and Node with TypeScript stripping.

## Install

Check out the package at the commit qualified for your installation:

```sh
git clone --filter=blob:none --sparse https://github.com/burin-labs/harn .smart-ship-source
git -C .smart-ship-source sparse-checkout set personas/smart_ship
git -C .smart-ship-source checkout <qualified-commit>
harn add .smart-ship-source/personas/smart_ship
harn install
harn package doctor
```

Import `harn-smart-ship/lib` from your project's Harn code. To distribute only
the package files, create an artifact directory with
`harn package pack .smart-ship-source/personas/smart_ship --output vendor/smart-ship`
and add that directory instead. Preserve its qualified source identity and hash.

Create an installation JSON document using the
[`Installation` reference](api.md#type-installation). Supply your repository names,
public verifier and publisher keys, budgets, credential-free Git state remote,
state branch, local bare workspace, and pinned controller artifact digest.
Keep private signing keys in your credential store. Configure Git authentication
and commit signing through Git's normal host configuration.

## Initialize and inspect

Initialization creates shadow authority, which has no admission, repair, rerun,
or deployment authority:

```sh
harn run .smart-ship-source/personas/smart_ship/cli.harn -- '{"action":"init","configuration":"smart-ship.json"}'
harn run .smart-ship-source/personas/smart_ship/cli.harn -- '{"action":"inspect","configuration":"smart-ship.json"}'
```

Inspection reports durable authority, checkpoints, active and pending snapshots,
incidents, external requests, and verification counts and failing names.
`verification: null` means no job inventory has been frozen.

## Control

After your effect adapter and release consumer are qualified, activate the owner:

```sh
harn run .smart-ship-source/personas/smart_ship/cli.harn -- '{"action":"control","configuration":"smart-ship.json","mode":"active"}'
```

Use `mode: stopped` to revoke repair claims and stop new work. Resume explicitly
with `mode: active`; stopped workers must claim a new attempt. Use `mode: draining`
to refuse new admission while already admitted verification finishes.

To transfer ownership, drain first and reconcile every reserved or unknown
external request. Invoke `action: transfer` with `target_controller` set to the
new qualified artifact digest. The new generation starts in shadow mode. Update
the installation's controller digest before activating it. A rollback follows
the same transfer procedure with the prior qualified artifact and a new generation.

## Uninstall

Disable the installation's event triggers, drain admitted work, and reconcile
external requests. Set `mode: stopped` and inspect the confirmed durable state.
Remove the `harn-smart-ship` dependency from `harn.toml`, then run `harn install`.
Retain remote state and signed checkpoint history for release consumers and recovery.
