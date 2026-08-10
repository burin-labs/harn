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

The policy is also the source of truth for metered runner rates, source URLs,
and their effective date; the approximate costs below are projections from
that data. GitHub rounds each job to a whole minute. See
[Actions runner pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing),
the [larger runner reference](https://docs.github.com/en/actions/reference/runners/larger-runners),
and [Blacksmith pricing](https://www.blacksmith.sh/pricing).

## Current decision

Use `blacksmith-12vcpu-macos-15` for primary `x86_64-apple-darwin` builds and
exact-source macOS workspace certification, and use the independent
GitHub-hosted `macos-15-xlarge` runner for release-archive recovery. Keep
`warm` and `standard` on `macos-15-intel`. Routine main pushes and scheduled
cache refreshes stay free; workflow-dispatched certification, shipping, and
recovery use paid capacity.

Set the repository Actions variable `HARN_RELEASE_FORCE_STANDARD_MACOS=true`
to route policy-selected primary and recovery Intel builds back to
`macos-15-intel` without a code change. Unset the variable, or set it to
`false`, to restore the checked-in policy. Explicit `standard` and `fast`
recovery or benchmark profiles still honor the operator's selected profile.
Set `HARN_RELEASE_FORCE_STANDARD_LINUX=true` to move release and benchmark CLI
AOT preparation from Blacksmith back to `ubuntu-latest`; warm-cache runs use
`ubuntu-latest` regardless. These variables are kill switches, not routine
profile selectors.

The primary and recovery choice is based on a same-source cold-cache pair.
Both runs used Harn source commit
`34e00360c1f310b364b92dd2e4aeabeddde46528`, the same target, AOT payload,
thin-LTO profile, and 16 codegen units. The branches differed only in runner
policy, and both reported `Swatinem cache hit: false`.

| Receipt | Runner | Cargo duration | Job duration | Cost |
| --- | --- | ---: | ---: | ---: |
| [GitHub M2 benchmark](https://github.com/burin-labs/harn/actions/runs/31321615097) | `macos-15-xlarge` | 10m01s | 10m25s | about $1.12 before credits |
| [Blacksmith M4 benchmark](https://github.com/burin-labs/harn/actions/runs/31321636531) | `blacksmith-12vcpu-macos-15` | 5m30s | 5m53s | about $0.96 |

Blacksmith saved 4m31s of Cargo time, or 45.1%, and 4m32s of job wall time over
the credit-eligible GitHub M2 recovery. More importantly, it cut 18m39s from
the previously selected cache-hit Intel Large job. Harn published 60 v0.10.x
releases in the 30 days ending 2026-08-09. At that unusually high cadence, the
measured Blacksmith macOS job projects to about $58 per month. The 16-vCPU
Blacksmith AOT job adds about $6 per month at the same cadence.

A second [Blacksmith validation run](https://github.com/burin-labs/harn/actions/runs/31322262014)
identified the cross-compiled output as an x86_64 Mach-O and executed
`harn --version` under Rosetta. The downloaded artifact independently passed
the same execution check, then passed ad-hoc signing, strict signature
verification, and another x86_64 execution on an Apple Silicon host.

CLI AOT preparation used to take 4m57s on standard Linux in the same benchmark
cohort. A controlled
[16-vCPU Blacksmith run](https://github.com/burin-labs/harn/actions/runs/31322214994)
finished that job in 2m26s. A GitHub 16-core trial was abandoned after five
minutes without runner assignment, so it is not a production fallback. The
measured AOT plus primary macOS job is about 8m19s before the sub-minute signing
and notarization tail; this is a component-path estimate, not a completed
release SLO claim.

Cache availability remains the larger lever. The v0.10.67 release
[missed its cache](https://github.com/burin-labs/harn/actions/runs/31312058230)
and spent 79m05s in Cargo on standard Intel. A warm standard runner cut that to
36m38s without paid capacity. The ARM runners complete cold builds faster than
that warm Intel path, so primary release latency no longer depends on an x86
cache hit. Intel cache warms remain useful for the emergency standard fallback
and retain the repository's existing storage budgets and pruning.

An earlier cold-cache benchmark saved only 55 seconds on Large. That result
correctly blocked adoption at the time, but cold dependency compilation is not
the intended steady-state Intel release path. The later cache-hit pair showed a
36.1% Intel Large advantage and justified the first paid default; the controlled
ARM pair above supersedes it with a faster and cheaper primary.

Update this table and any `primary` label change only from an observed
workflow/job receipt. Force standard capacity if Blacksmith has two consecutive
runner/platform failures, if the primary job p95 exceeds 10 minutes over five
releases, or if projected release-runner spend exceeds $150 per month without a
matching release cadence. Do not infer a capacity win from runner
specifications alone.
