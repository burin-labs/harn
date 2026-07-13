# Release runner policy

Release binary runner labels are data, not workflow control flow. The source of
truth is `.github/release-runner-policy.json`; `scripts/release_runner_matrix.sh`
validates that document and resolves the matrix consumed by
`build-release-binaries.yml`.

Each target declares five labels:

- `warm`: routine default-branch cache refreshes.
- `primary`: tag-push release builds on the product critical path.
- `recovery`: manual tag recovery when no override is requested.
- `standard`: an explicit standard-capacity recovery or benchmark override.
- `fast`: an explicit latency-prioritized recovery or benchmark override.

Primary and warm builds always use policy. Recovery accepts `policy`,
`standard`, or `fast`; the default is `policy`. The non-publishing benchmark
mode requires an explicit target subset and either `standard` or `fast`. It
compiles and runs the binary-size gate, but cannot sign, notarize, package,
upload, finalize a release, publish a container, or save a cache.

```bash
gh workflow run build-release-binaries.yml \
  --ref <branch> \
  -f benchmark_only=true \
  -f runner_profile=fast \
  -f targets=x86_64-apple-darwin
```

The policy records the pricing source and effective date beside the labels.
GitHub currently lists the 12-core Intel macOS Large runner at $0.077 per
rounded-up minute; larger runners are billed for public repositories. See
[Actions runner pricing](https://docs.github.com/en/enterprise-cloud@latest/billing/reference/actions-runner-pricing)
and the [larger runner reference](https://docs.github.com/en/actions/reference/runners/larger-runners).

## Current decision

The recent standard Intel baselines are 41m35s for v0.10.13 and 62m58s for
v0.10.14. The primary policy remains standard until the controlled Large-runner
receipt below is complete. Recovery and warm policy remain standard regardless;
operators may request `fast` for a targeted recovery when latency justifies the
billed spend.

| Candidate | Runner | Build duration | Job duration | Estimated cost |
| --- | --- | ---: | ---: | ---: |
| Standard baseline | `macos-15-intel` | 41-63 min | 41-63 min | public-repo standard policy |
| Controlled fast benchmark | `macos-15-large` | pending | pending | `ceil(job minutes) * $0.077` |

Update this table and the `primary` label only from an observed workflow/job
receipt. Do not infer a capacity win from runner specifications alone.
