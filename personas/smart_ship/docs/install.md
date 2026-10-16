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
public verifier, scope, admission and publisher keys, budgets, credential-free Git state remote,
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
For a policy change, include the complete replacement `next_policy` in the
transfer request. The state owner validates and archives that policy alongside
its mechanically checked digest before committing the new generation. Update
the installation configuration after that committed transfer.

## Observe GitHub reviews

Import `harn-smart-ship/github` to call `read_github_reviewed_change`. Its request names
the repository, pull request, exact expected head, and bounded page limits.
Credentials enter through your host's secret store. The connector remains the
owner of authenticated GitHub reads.

For the read-only command, save a `GithubPullRequestReviewCensusRequest` in
`review-request.json` and supply the secret reference name, never its value:

```sh
HARN_EGRESS_ALLOW=api.github.com harn run --allow-process-network --read-only-root review-request.json .smart-ship-source/personas/smart_ship/github-cli.harn -- live review-request.json github/access-token > review-observation.json
harn run --read-only-root review-observation.json .smart-ship-source/personas/smart_ship/github-cli.harn -- replay review-observation.json
```

Live reads refuse a changed head or incomplete inventory. Replay uses the same
source and inventory validation. Both preserve source author, lifecycle, draft
status, repository names and immutable IDs, review commits, submission and edit
times, platform approval, and thread resolution separately. Unknown facts stay
unknown.
The observation supplies review evidence; it does not admit or merge the change.

## Integrate a qualified GitHub change

Qualify the release guard and transfer exclusive landing ownership before
activating this adapter. GitHub continues to enforce effective branch rules,
including a required merge queue. The command does not change repository rules.

Save a `GithubAdmissionConfiguration` in a trusted file outside submitted
artifacts. Its `installation` field contains your qualified `Installation`
document; `token_reference` names a host secret, and `method` selects `merge`,
`squash`, or `rebase`. Keep Git authentication with the host's normal credential
store. Save the signed witness and current authority fence as a
`GithubAdmissionSubmission` in `submission.json`.

```sh
HARN_EGRESS_ALLOW=api.github.com,github.com harn run --allow-process-network .smart-ship-source/personas/smart_ship/github-effect-cli.harn -- github-effect.json submission.json
```

Inspect `dispatch` and the typed `admission_refusal`. A confirmed dispatch
retains the provider's integrated commit in the durable receipt's JSON body.
Use that actual commit and resolve its tree when forming the next verification
snapshot. Keep the release checkpoint held until complete verification passes.
An uncertain dispatch requires reconciliation; submitting the same admission
again reads its recorded outcome without contacting GitHub. A stop or stale
fence prevents a new dispatch, while an already committed dispatch can finish
and retain its measured result.
If a process stopped after reserving an admission but before claiming its
dispatch, inspect the current authority fence and resubmit the same signed
witness with that fence. The existing owner claims the reservation once.
An operation already marked `dispatching` or `unknown` requires reconciliation.

## Uninstall

Disable the installation's event triggers, drain admitted work, and reconcile
external requests. Set `mode: stopped` and inspect the confirmed durable state.
Remove the `harn-smart-ship` dependency from `harn.toml`, then run `harn install`.
Retain remote state and signed checkpoint history for release consumers and recovery.

## Prepare verification

Before integrating a change, obtain a `SignedScope` from the trusted owner of
job selection. Match its rule to your frozen policy's `admission_rules`; each
rule binds a repository, optional immutable repository ID, destination
`base_branch`, scope trust domain,
and independent review policy. Configure GitHub rules with the observed base
repository ID. Plain Git installations may explicitly use `repository_id: null`.
Candidate code must not choose the scope inventory or receive scope/admission
signing credentials.
The scope owner binds `reviewed_head`, `reviewed_base` and
`reviewed_base_branch` to the actual tested `source` and destination branch.
Keep GitHub's generated test merge commit there; it must not be
relabelled as the reviewed PR head or the workflow run's API head field.

Build an `AdmissionRequest` from that scope and the complete reviewed source
observation, retaining the full frozen `policy` and its digest. Verification
requires that embedded policy to match the independently trusted installation
policy; a receipt's own keys do not establish trust. Call `prepare_admission` to obtain the complete precheck census, and
bind prepared stock jobs with `bind_admission_job` before execution. Retain the original signed CI proofs
in an `AdmissionRecord`, sign `admission_bytes` with the configured admission
issuer, and require `verify_admission(...).status == admitted` before integrating.
Missing checks remain pending; incomplete scope, invalid provenance, invalid
reuse, or mismatched source/policy/issuer cannot qualify. Store the full signed
record in `Change.admission`; grouped snapshots and release consumers recheck it.

Build external admission requests with `admission_effect(signed)`. Pass that
request to `reserve_effect` with your current state, fence, and trusted policy.
The reservation checks the complete record and its derived target and digest.
Commit the returned state through the Git compare-and-swap owner before calling
`dispatch_reserved_effect`. An unsigned digest cannot authorize admission.
Release requests must name the exact `checkpoint_id` in `input_digest` and a
source in that fully verified checkpoint.
Read admission refusals through `EffectRefusal.admission` and checkpoint
refusals through `EffectRefusal.verification`. These retain required counts,
pending and failing job names, and invalid evidence. A nil field means that
qualification was not measured for that stage.

Before signing a cumulative candidate, measure its tree with `prepare_group_tree`
using the exact destination base and ordered reviewed heads. Call
`require_group_ancestry` with each original reviewed base in the same head order:
an older reviewed base must remain an ancestor of both the current destination
and its original head. Main advancing does not rewrite an admission signature.
Build
`candidate_effect(repository, base, tree, admissions)` with those complete signed
admissions, then reserve it through `reserve_effect` with the current time and
commit that state before dispatch. The digest preserves admission order. Read
`EffectRefusal.preparation` for the required count, every admission decision and
invalid input names. Empty or duplicated input is refused.

Candidate preparation uses the same durable dispatch owner as other effects.
An unknown signing result forbids redispatch and blocks authority transfer until
read-back reconciliation. Confirmation records the preparation receipt only;
it does not offer a verification snapshot, move the destination, or promote a
release checkpoint. The GitHub candidate-signing adapter and its recovery remain
required before this reservation can become the default integration route.

Each authority generation freezes `policy_digest`. To change policy, drain
pending and active verification, reconcile external effects, and resolve work
that still needs the old policy. Call `transfer` with the next policy, commit
and read back the new shadow generation, then update the installation config
before activating it. Initialization refuses an untransferred config change.
Read-only inspection can still show a checkpoint under its archived policy.

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
