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
Measure the installed artifact with its own entrypoint:

```sh
harn run vendor/smart-ship/artifact-cli.harn
```

Use the returned `digest` as the installation's `controller`. This measures the
executing module's package through Harn's canonical package inventory, including
its dependency lock. A source checkout and a packed artifact can have different
packaging metadata, so measure the exact artifact you will run. Changing any
inventoried byte requires a newly qualified digest and ownership transfer.

Create an installation JSON document using the
[`Installation` reference](api.md#type-installation). Supply your repository names,
public verifier, scope, admission and publisher keys, budgets, credential-free Git state remote,
state branch, local bare workspace, and pinned controller artifact digest.
Keep private signing keys in your credential store. Configure Git authentication
and commit signing through Git's normal host configuration.
The required `writer_enrollment` field declares the initial former-writer
inventory. Use `null` only for a fresh installation with no existing writer;
otherwise supply the complete `WriterEnrollment`. Initialization records that
declaration in its first authority commit. A missing field is refused, and
later configuration changes cannot erase the recorded inventory.
The control and grouped GitHub entrypoints refuse a different executing package
before authority writes or credential reads. `inspect` remains read-only and
available with a mismatched pin. Embedded hosts must bind their own executing
artifact before granting controller capabilities; a digest string alone is not
runtime attestation.
When configuration or state lives outside the package, grant the configuration
with `--read-only-root` and the private state workspace with `--write-root` on
`harn run`. Git remotes also require the operator's process-network permission.
Keep those grants separate from the restricted repair-worker workspace.

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

If another shipping or recovery writer already operates the repository, enroll
it before activating this controller. `enroll-writers` takes a
`writer_enrollment` containing the complete `writers` inventory, the trusted
scope observer's `scope_trust_domain`, and `max_drain_age_ms`. Each writer names
its stable `id`, repository and exact control-source Git commit. Enrollment
belongs to the current shadow generation and is stored in controller authority.
An existing writer captures a `FormerWriterPermit` once with
`capture_former_writer`; it checks `former_writer_authorized` against freshly
read authority before every mutation. A delayed process must retain its original
permit rather than capturing a replacement generation.
The permit also binds the control revision. Stop and resume cannot restore a
stopped process's authority; a fresh process must capture a new permit.

For callers outside Harn, use the installation CLI's read-only `capture-writer`
action at process startup. Pass `configuration` and `writer`, whose fields are
the enrolled `id`, `repository`, and control-source `source` commit. Retain the
returned permit for that process. Before each mutation, call `authorize-writer`
with the same `configuration` and original `writer_permit`. The response names
the measured `state_commit` and returns an explicit `authorized` or `denied`
status. Proceed only on `authorized`; checking never captures a replacement
permit. These reads do not change authority or create an effect reservation.

Missing state, an unenrolled writer, stale policy, a repository hold, drain, and a changed
generation deny authorization. Invalid or unreadable configuration or state
fails the command. Neither result permits a fallback to the old route. Keep
the installation location mandatory for an enrolled repository, including when
its configuration file is missing. A refused check retains its original permit
for inspection; a refused capture returns no permit. Outstanding provider
effects must still settle before the signed drain can retire their owner.

Invoke `drain-writers` before collecting provider observations. This commits the
old route's admission stop. Wait for already-started effects and queued work to
settle; a missing runner or an observation timeout does not prove completion.
For every enrolled writer, measure the complete unfinished workflow, queue,
repair and uncertain-effect inventories. Each `WriterDrainCensus` retains the
actual measured resource identity, completeness, declared count and unsettled
item identities. A resource must be positively identified before its empty
inventory is trusted. These are provider observations, not values inferred from
the absence of local files.

The installation's trusted scope observer signs `writer_drain_bytes` for a
`WriterDrain` bound to that installation, controller, policy, generation and
durable drain revision. Use `qualify_writer_drain` to inspect required and
observed counts and named refusals. Pass the complete `SignedWriterDrain` as
`writer_drain` to `retire-writers`. The CLI uses its own current clock for the
validity check. Signing keys remain in the observer's secret store; the request
contains only the public proof. Missing writers, partial inventories, unfinished
work, expired observations and invalid signatures prevent retirement and
activation. A signature authenticates the observer; it does not establish that
the observer actually queried the provider correctly.

After your effect adapter and release consumer are qualified, activate the owner:

```sh
harn run .smart-ship-source/personas/smart_ship/cli.harn -- '{"action":"control","configuration":"smart-ship.json","mode":"active"}'
```

Use `mode: stopped` to revoke repair claims and stop new work. Resume explicitly
with `mode: active`; stopped workers must claim a new attempt. Use `mode: draining`
to refuse new admission while already admitted verification finishes.
Stop and shadow mode durably refuse external requests that never acquired a
dispatch claim. Already dispatched work remains unresolved until its provider
outcome is measured; resuming does not authorize another attempt at that request.

To transfer ownership, drain first and reconcile every reserved or unknown
external request. Invoke `action: transfer` with `target_controller` set to the
new qualified artifact digest. The new generation starts in shadow mode. Update
the installation's controller digest before activating it. A rollback follows
the same transfer procedure with the prior qualified artifact and a new generation.
Former-writer retirement survives stop, shadow mode and later transfers. To
restore an old route, first drain the current owner and transfer to a fresh
shadow generation, restore its protected repository policy, then explicitly
enroll that route again. Old permits remain invalid; a fresh old-writer process
must capture the new generation. This does not waive real provider-drain or
release-guard qualification.
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

## Observe a GitHub group

Use the observer before transferring shipping authority. It reads current
reviews and composes the original reviewed heads without reserving an effect,
signing a provider commit, pushing a ref, running submitted commands, or
advancing a checkpoint.

Save a `GithubGroupObserverConfiguration` separately from submitted evidence.
It contains the qualified executing artifact's `controller` digest, trusted
`policy`, credential-free Git `store`, and a host `token_reference`. Initialize
the store's private bare workspace with `git init --bare`. Its `ref` names the
planned authority branch only to prevent source-ref collisions; observation
does not initialize or write that branch. Give the GitHub credential read-only
repository and pull-request access.

Save a `GithubGroupObservationRequest` in `group.json`: `repository` names the
one GitHub repository and `admissions` contains the complete original signed
records, in composition order. A request cannot select its policy, credentials,
remote, or executing artifact.

```sh
harn run --standalone --no-sandbox vendor/smart-ship/github-observer-cli.harn -- observer.json group.json > observation.json
```

Run only this trusted pinned controller in host mode. Verification and repair
commands keep their separate restricted execution environments.

Read the structured `status`, not the command's exit code. `blocked` retains
required and observed change/review counts, named pending, failed, and invalid
checks, original admission decisions, provider read errors, and available review
censuses. Missing reads stay pending. An admission signed under another policy
cannot be relabelled for the current one. Invalid original evidence is refused
before credentials are read.

`composable` retains the observed destination and exact proposed `GroupTree`.
The observer checks the destination again after composition. This is a forecast;
it neither proves combined tests pass nor authorizes publication. The publisher
still obtains fresh reviews and guards actual refs when dispatching integration.
For archived observations, `assess_github_group` uses the same admission and
current-head qualifiers; its `qualified` result has not performed Git composition.

## Prepare and integrate a GitHub group

Qualify the release guard and transfer exclusive landing ownership before
activating this adapter. The command does not change repository rules. Its
atomic publication must be permitted by the installation's effective policy;
a required merge queue remains authoritative until a qualified fenced transfer.

Save a `GithubIntegrationConfiguration` in a trusted file outside submitted
artifacts. Its `installation` field contains your qualified `Installation`
document; `token_reference` names a host secret. Requests cannot choose
credentials, a remote, or another API endpoint. Keep Git authentication and
authority commit signing in the host's normal configuration.

The controller is privileged: it holds Git credentials and the provider secret
handle. The default worktree sandbox intentionally denies SSH credential files.
The live signing proof runs only the pinned controller in trusted host mode;
verification and repair commands run separately with their own restricted
capabilities. Do not execute submitted commands in this controller invocation.

Measure the cumulative tree with `prepare_group_tree`, then construct the exact
`CandidateEffect` with `candidate_effect(repository, base, tree, admissions)`.
Save a `GithubCandidateCommand` with `action: prepare`, that request, and the
current authority `fence` in `command.json`. Keep the complete original signed
admissions in their original order. The command commits the reservation before
claiming its unique signing dispatch.

```sh
harn run --standalone --no-sandbox .smart-ship-source/personas/smart_ship/github-effect-cli.harn -- github-effect.json command.json
```

Inspect `reservation` and `candidate` in `GithubCommandResult`. A refused or
unknown reservation has no candidate outcome. A confirmed candidate retains its
actual signed commit, tree and parent; main and the release checkpoint have not
advanced. An unknown signing result requires `action: recover-candidate` with
the same request. Recovery observes the unique recorded dispatch and signed
object without creating another commit. Resume an undispatched reservation by
passing the same request and a fresh observed fence to `prepare`.

Build the verification snapshot from the actual candidate identity and measured
stock-job plans. Retain every original head and signed admission in its
`Change`, with `integrated_commit` set to the candidate. Construct
`integration_effect(snapshot)`, then save a `GithubIntegrationCommand` with
`action: integrate`, that request, the current `fence`, and `expected_base` set
to the candidate's original destination base. The same CLI reserves and
dispatches the atomic destination, authority and reviewed-head transaction.
Successful integration creates pending verification; the release checkpoint
stays held until complete verification passes.

After an uncertain publication, use `action: recover-integration` with the same
integration request and `expected_base`. Inspect `reconciliation.status`.
Use `cleanup-integration` only after the outcome is conclusive; it deletes the
exact temporary state-object ref and preserves unresolved objects. Missing
outcome fields mean that stage was not measured. Keep the complete command
result and remote authority history across restarts.

After confirmed integration, use `action: cleanup-candidate` with the original
candidate `request` and the confirmed `integration` request. Cleanup rechecks the
signed identity and requires the destination to retain that candidate. It deletes
only the exact owned candidate ref, guarding the measured authority and destination
in the same transaction. An unresolved integration or a changed destination retains
the branch. Repeat the command to observe an uncertain deletion; an already absent
branch does not cause another provider mutation.

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
release checkpoint. The GitHub CLI performs reservation and dispatch through
the same owner; embedded callers can use those lower-level functions directly.

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
