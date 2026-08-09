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

Use `macos-15-large` for primary and recovery `x86_64-apple-darwin`
builds. Keep `warm` and `standard` on `macos-15-intel`. Routine main pushes and
scheduled cache refreshes therefore stay free; only a shipping or recovery job
uses paid capacity.

Set the repository Actions variable `HARN_RELEASE_FORCE_STANDARD_MACOS=true`
to route policy-selected primary and recovery Intel builds back to
`macos-15-intel` without a code change. Unset the variable, or set it to
`false`, to restore the checked-in policy. Explicit `standard` and `fast`
recovery or benchmark profiles still honor the operator's selected profile.

The policy is based on the cache-hit pair below. Both benchmark runs used
v0.10.67 commit `64321c120a119ac32cae267831a3e54cd56a6ec8`, the same
target, AOT payload, thin-LTO profile, and 16 codegen units. Both reported
`Swatinem cache hit: true`.

| Receipt | Runner | Cargo duration | Job duration | Larger-runner cost |
| --- | --- | ---: | ---: | ---: |
| [Standard benchmark](https://github.com/burin-labs/harn/actions/runs/31319356648) | `macos-15-intel` | 36m38s | 38m13s | $0 |
| [Large benchmark](https://github.com/burin-labs/harn/actions/runs/31317916273) | `macos-15-large` | 23m25s | 24m32s | about $1.93 |

Large saved 13m13s of Cargo time, or 36.1%, and 13m41s of job wall time. This
clears the adoption threshold of both 10 minutes and 30% under equivalent cache
state. Harn published 60 v0.10.x releases in the 30 days ending 2026-08-09. At
that unusually high cadence, the measured job projects to about $116 per month
and removes about 13.7 hours of aggregate release critical-path waiting.

Cache availability remains the larger lever. The v0.10.67 release
[missed its cache](https://github.com/burin-labs/harn/actions/runs/31312058230)
and spent 79m05s in Cargo on standard Intel. A warm standard runner cut that to
36m38s without paid capacity; Large removes the remaining capacity-sensitive
tail. Cache warms therefore stay on the free standard runner and retain the
repository's existing storage budgets and pruning.

An earlier cold-cache benchmark saved only 55 seconds on Large. That result
correctly blocked adoption at the time, but cold dependency compilation is not
the intended steady-state release path. The cache-hit pair above changes one
variable and measures the path the default-branch warmer exists to provide.

Update this table and any `primary` label change only from an observed
workflow/job receipt. Revert to standard if two controlled cache-hit pairs show
less than a 10-minute or 30% Cargo advantage, or if projected larger-runner
spend exceeds $150 per month without a matching release cadence. Do not infer a
capacity win from runner specifications alone.
