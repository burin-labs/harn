# Use the CI recovery workflow

Call Harn's reusable workflow to retry a CI run whose runner lost its job verdict.
The controller reads GitHub metadata and logs without executing the failed run's code.

Add a default-branch trigger like this. Replace both `HARN_COMMIT_SHA` values with the same reviewed Harn commit.
[GitHub supports commit pins for reusable workflows](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows).

```yaml
name: CI recovery
on:
  workflow_run:
    workflows: [CI]
    types: [completed]
permissions:
  actions: write
  contents: write
  pull-requests: write
concurrency:
  group: ci-recovery-${{ github.event.workflow_run.id }}
  cancel-in-progress: false
jobs:
  repair:
    if: ${{ github.event.workflow_run.conclusion == 'failure' || github.event.workflow_run.conclusion == 'cancelled' }}
    uses: burin-labs/harn/.github/workflows/ci-preemption-recovery.yml@HARN_COMMIT_SHA
    with:
      orchestration-sha: HARN_COMMIT_SHA
      run-id: ${{ github.event.workflow_run.id }}
      policy-json: >-
        {"schema":"harn.ci_preemption_policy.v1",
         "aggregate_job_names":["CI status"],
         "timeout_completion_grace_seconds":90,
         "lost_verdict_routes":[{"workflow":"CI","job_names":["Tests"],
                                 "events":["pull_request","merge_group"]}]}
```

Declare only jobs that can safely repeat. Use their exact GitHub display names.
List the originating run's events, rather than `workflow_run`, in `events`.
Keep publication, deployment, and other irreversible jobs outside this list.

The controller retries lost verdicts only on attempt one.
It requires a completed failed job, successful preceding steps, one running step,
and later pending steps without conclusions or timestamps.
An available log, a failed step, incomplete metadata, or mixed failure evidence prevents this retry.
The replacement attempt supplies the authoritative workload verdict.

Inspect the job summary or `ci-recovery-<run-id>` artifact for classification,
counts, evidence completeness, and planned action.
`inspected_job_count` counts downloaded logs; a lost verdict instead has complete metadata evidence and no log.
The same policy retains existing runner-preemption and configured-timeout recovery.
Those paths can requeue a merge group; lost verdicts rerun failed jobs directly.
