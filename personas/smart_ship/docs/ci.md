# Run a stock verification job in CI

Use this command from a trusted workflow with a qualified Harn runtime, Git,
and the project's existing test tools. Install the qualified Smart Ship package
as described in [Install and control Smart Ship](install.md). The command is
included in the package artifact as `job-cli.harn`.

## Prepare the command

Save a public `prepare.json` request. This example uses Node's existing test
runner and JUnit reporter:

```json
{
  "action": "prepare",
  "job": {
    "installation": "my-project",
    "id": "tests",
    "repository": "my-project",
    "workspace": "/workspace/project",
    "command": {
      "kind": "tests",
      "program": "node",
      "args": [
        "--test",
        "--test-reporter=junit",
        "--test-reporter-destination=.ship-report.xml"
      ],
      "version_args": ["--version"],
      "timeout_ms": 300000,
      "environment": { "PATH": "/usr/local/bin:/usr/bin:/bin" },
      "test_report": ".ship-report.xml"
    },
    "outputs": [],
    "trust_domain": "my-ci",
    "run": "ci-run-123",
    "attempt": 1
  }
}
```

Set the workspace, executable, environment, run and attempt to the actual job.
Declare required outputs as `{name, path}` entries. Ignore generated reports
and outputs in Git; preparation requires a clean checkout. The child receives
only the declared environment, so include the tools' required variables.

```sh
harn run --write-root /workspace/project vendor/smart-ship/job-cli.harn -- prepare.json > job-plan.json
```

Preparation measures the exact Git commit and tree, command, toolchain,
platform, environment and full-tree coverage. It does not run tests or issue
an admission decision. Its execution request is empty until the controller
binds it to a frozen dispatch.
Grant the actual project directory as the write root, even when the installed
package lives elsewhere. Keep public request files there or grant each external
request file with `--read-only-root`.

## Execute the frozen dispatch

Obtain the independently prepared dispatch from your trusted controller.
It is either `{kind: "snapshot", snapshot: <frozen Snapshot>}` or
`{kind: "admission", request: <AdmissionRequest>}`. Keep scope and policy
selection outside candidate code. The command verifies that the prepared job
matches this dispatch; it does not establish the controller's authority or
qualify reviews and policy on its behalf.
For admission, the dispatch binds the signed scope and exact test inputs.
Reviews may finish later without rerunning the same verified command. The
admission issuer must qualify and sign the current reviews before admitting
the change. A changed scope, source or policy cannot borrow that execution.

Assemble `execute.json` with these fields:

| Field           | Value                                                     |
| --------------- | --------------------------------------------------------- |
| `action`        | `"execute"`                                               |
| `plan`          | The complete prepared `JobPlan` from `job-plan.json`      |
| `dispatch`      | The complete controller dispatch                          |
| `signer.id`     | The executor identity declared by the installation policy |
| `signer.secret` | A host secret reference such as `smart-ship/executor`     |

Make that reference available through the host's Harn secret store. A process
host can use the bounded parent-secret channel documented in the
[CLI reference](https://harnlang.com/docs/cli-reference.html). Never put a
private key in either request, an argument, the child environment, or the
repository. Candidate commands must not receive scope, admission or checkpoint
signing authority.

```sh
harn run --write-root /workspace/project vendor/smart-ship/job-cli.harn -- execute.json > execution.json
```

The command binds the dispatch before reading the executor secret, then uses
the same stock executor as local library callers. Execution remeasures the
source and inputs before starting the command. A changed checkout or mismatched
dispatch is refused. An execution that starts returns a `JobRun` containing
the signed original proof, diagnostics and observed exit code. A failed,
cancelled or infrastructure-failed result also exits nonzero; preserve
`execution.json` on that path. A refusal before execution produces no proof.

Submit the original proof to the controller. It retains the measured source,
dispatch identity, run, attempt, job, output digests and observed test counts.
Missing, empty or invalid reports cannot produce a passing test proof.
Do not replace original provenance when reusing evidence. The stock executor
uses full-tree coverage and disables reuse; a sound owning coverage adapter is
required before widening that policy.

This command does not merge a change, advance a checkpoint, publish a release,
or prove that GitHub ran a job. A GitHub workflow must retain its provider run
and job observations through the shared CI evidence adapter as well.

## Inspect original GitHub CI and issue admission

Use `admission-cli.harn` from a trusted workflow. Keep its request file,
installation policy, scope signer and job selection outside the candidate
checkout. The command reads GitHub and signs evidence; it does not change
branches, rules or releases.

Before running stock commands, call `github_ci_identities(request, jobs)` from
the package's GitHub export. Pass that exact identity list to the shared CI
registration action. Register the clean tested commit and tree before the
job legs start. Each leg must use `job-cli.harn` with its actual numeric
GitHub run ID as a string and its actual attempt number. Upload each complete
`JobRun` as a single JSON member using the declared artifact name.

Keep a trusted `issuer.json` configuration beside the installation's public
`policy.json`. Pass this configuration separately from every submitted or
downloaded admission file. Its fields are:

| Field | Value |
| --- | --- |
| `policy_file` | Path to the canonical public policy, relative to `issuer.json` |
| `producer_policy` | Trusted repository, workflow filename, allowed events, registrar job and step |
| `run_name` | The trusted workflow's display name |
| `jobs` | Logical job IDs and their complete list of exact leg bindings |
| `token_secret` | A read-only GitHub credential reference |
| `issuer` | The enrolled issuer's `{id, secret}` reference |

The submitted `admission.json` contains only:

| Field | Value |
| --- | --- |
| `action` | `"inspect"` or `"issue"` |
| `request` | The complete `AdmissionRequest` with its signed scope |
| `producer.run_id` | The original executing GitHub run ID |
| `producer.attempt` | Its original attempt number |

A job binding contains `id` and `legs`. Each leg names its `requirement`,
exact GitHub job `name`, `artifact_name`, and JSON `member_name`. Bind every
scope requirement once. A logical matrix job requires every declared leg;
one successful leg cannot stand in for the rest.

```sh
harn run --allow-process-network vendor/smart-ship/admission-cli.harn -- issuer.json admission.json > admission-result.json
```

Grant the configuration, public policy and request files as read roots, and a scratch directory as a write
root when they are outside the runner's workspace. Supply credentials through
the host secret store or bounded parent-secret stdin channel. Candidate
commands must never receive the GitHub token or admission issuer key.

The result retains `observation` with the provider's complete job census,
registration receipt, downloaded artifact receipts, signed stock results,
and assessment. The API workflow head stays separate from the tested commit
and tree. Original run, attempt, job IDs, source and input identities must
agree. A submitted policy must match the independently loaded installation
policy before any credential read. The command collects all CI observations,
then reads current exact-head reviews as its final provider read. It
requalifies the measured CI archive with those current reviews before signing,
without another network read. A changed head or base refuses the old signed scope.

`inspect` returns `admission: null`. `issue` reads the issuer key only after
all checks pass, then returns the signed admission alongside its observation.
Missing stock results stay pending even when GitHub reports success.
Pending or failed assessment prints the evidence and exits nonzero; a
structural or identity refusal produces no signed receipt. Keep the full
result for audit and replay through `assess_github_ci`.

The observation's `source_reads` retains the actual artifact-list and job-list
responses, including unrelated artifacts and every measured page.
`replay_github_ci(trusted_policy, observation, producer_policy, jobs)` requalifies those exact
observations through the same shared reader. Missing, reordered or unused
responses refuse. Replay checks the archived decision; it does not claim a
fresh provider read or grant permission to issue a live admission.

Decode a saved observation with `schema_of(GithubCiAssessment)` before replay.
Each artifact has a `status`: `"read"` carries its complete `receipt`, and
`"unavailable"` carries the connector's exact `error`. Preserve both outcomes
when storing the JSON. An absent rate-limit field stays absent; an explicitly
reported zero stays zero. An unavailable artifact cannot count as an executed
stock requirement.
