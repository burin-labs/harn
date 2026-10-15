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
