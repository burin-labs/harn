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

## Observe GitHub reviews

Import `harn-smart-ship/github` to call `read_github_reviews`. Its request names
the repository, pull request, exact expected head, and bounded page limits.
Credentials enter through your host's secret store. The connector remains the
owner of authenticated GitHub reads.

For the read-only command, save a `GithubPullRequestReviewCensusRequest` in
`review-request.json` and supply the secret reference name, never its value:

```sh
harn run --allow-process-network --read-only-root review-request.json .smart-ship-source/personas/smart_ship/github-cli.harn -- live review-request.json github/access-token > review-observation.json
harn run --read-only-root review-observation.json .smart-ship-source/personas/smart_ship/github-cli.harn -- replay review-observation.json
```

Live reads refuse a changed head or incomplete inventory. Replay uses the same
source and inventory validation. Both preserve unknown actors and review commits,
platform approval, thread resolution, and outdated-source status separately.
The observation supplies review evidence; it does not admit or merge the change.

## Uninstall

Disable the installation's event triggers, drain admitted work, and reconcile
external requests. Set `mode: stopped` and inspect the confirmed durable state.
Remove the `harn-smart-ship` dependency from `harn.toml`, then run `harn install`.
Retain remote state and signed checkpoint history for release consumers and recovery.

## Prepare verification

Call `prepare_job` for each declared stock command, then build the complete
snapshot from those measured sources and requirements. Call `freeze_snapshot`
before offering or starting it. Bind each preparation with `bind_job` and pass
that plan to `execute_job` after the durable `start` transition is confirmed.
Execution rechecks source and toolchain identity before running the command.

Use `run_job` for diagnostic checks during a repair. Those checks retain signed
provenance, but they cannot substitute for a controller snapshot's independently
dispatched verification. A later snapshot must identify a prior proof as reused
and its qualified scope policy must permit reuse.

## Guard a release

Before publication, hash the actual artifact bytes and name their exact source
commit and tree. Put that source and the artifact names and digests in a
`ReleaseRequest`, together with the pinned checkpoint digest. Supply the actual
output paths as `files`; the guard reads and hashes them. Invoke it against the
signed checkpoint file:

```sh
harn run .smart-ship-source/personas/smart_ship/cli.harn -- '{"action":"release-guard","configuration":"smart-ship.json","release":{"checkpoint_file":"checkpoint.json","request":{"checkpoint":"sha256:<checkpoint-digest>","source":{"repository":"project","commit":"<commit>","tree":"<tree>"},"artifacts":[{"name":"binary","digest":"sha256:<artifact-digest>"}]},"files":[{"name":"binary","path":"dist/binary"}]}}'
```

The guard refuses invalid signatures, incomplete verification, another checkpoint,
a changed source, missing outputs, substituted bytes, or ambiguous producers.
Successful output retains each original execution's provenance, including reuse.
Publish those same immutable bytes through your deployment adapter and record
its actual receipt separately. The guard measures files at invocation; it does
not lock them against later modification or report that deployment succeeded.
