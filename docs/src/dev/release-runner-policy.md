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

Keep primary, recovery, and warm policy on standard runners. Retain `fast` only
as an explicit targeted benchmark/recovery override. All three measured builds
below reported `Swatinem cache hit: false`, so the comparison does not hide a
cache-state advantage.

| Receipt | Runner | Cargo duration | Job duration | Estimated larger-runner cost |
| --- | --- | ---: | ---: | ---: |
| [v0.10.13](https://github.com/burin-labs/harn/actions/runs/29206955853) | `macos-15-intel` | 39m27s | 41m35s | $0 |
| [v0.10.14](https://github.com/burin-labs/harn/actions/runs/29213650285) | `macos-15-intel` | 62m45s | 65m53s | $0 |
| [Controlled fast benchmark](https://github.com/burin-labs/harn/actions/runs/29217171568) | `macos-15-large` | 38m32s | 39m27s | $3.08 |

The Large runner saved only 55 seconds of Cargo time and 2m08s of total job
time against the recent typical standard run, while billing 40 rounded minutes.
It was faster than the anomalous v0.10.14 tail, but that variance does not
justify a paid default. The asynchronous release watcher prevents the long
artifact lane from blocking the operator; use `fast` only when a specific
recovery's latency is worth the metered spend.

Update this table and any `primary` label change only from an observed
workflow/job receipt. Do not infer a capacity win from runner specifications
alone.
