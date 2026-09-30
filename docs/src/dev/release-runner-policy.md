# Release runner policy

Release binary runner labels are data, not workflow control flow. The source of
truth is `.github/release-runner-policy.json`; `scripts/release_runner_matrix.sh`
validates that document and resolves the matrix consumed by
`build-release-binaries.yml`.

Each target declares five labels:

- `warm`: routine default-branch cache refreshes.
- `primary`: release candidate builds on the product critical path (the
  version commit's push to main; candidate mode resolves to these labels).
- `recovery`: no workflow path selects it since releases are promoted from the
  candidate run instead of rebuilt; `scripts/release_runner_matrix.sh` still
  validates it.
- `standard`: an explicit standard-capacity benchmark override.
- `fast`: an explicit latency-prioritized benchmark override.

Candidate and warm builds always use policy. The non-publishing benchmark
mode requires an explicit target subset and either `standard` or `fast`. It
compiles and runs the binary-size gate, but cannot sign, notarize, package,
upload, or save a cache.

The `source_candidate` dispatch input defaults to false. When true, it requires
the main branch and policy runners, excludes warm and benchmark inputs, and
uses the same five-target candidate builder, signing, notarization,
attestations, manifest and checks as a release candidate. Its context records
`candidate_purpose=source`, and its notes identify the unreleased commit.
Promotion accepts only stable version-changing main pushes, so this dispatch
does not publish a tag or release. The request policy lives in
`scripts/release_contract.harn`; the workflow passes a closed request to its
decision function and records the typed admission or refusal receipt. The
existing verified bootstrap supplies the decision interpreter. Only this
repository reads the source policy, so it is not projected into
`scripts/release_contract.json`, the contract the release orchestrator checks.
Scheduled source production and downstream artifact consumption are separate
from this explicit producer input.

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

Release candidates build both Apple targets on `macos-15-xlarge`, including
the x86_64 cross build. Linux targets and CLI AOT preparation use Blacksmith's
16-vCPU Ubuntu 22.04 image. GitHub's 16-core Ubuntu 22.04 pool is the independent
Linux recovery path. Windows remains on `windows-latest` because the measured
47m05s Windows job fits the 75-minute release objective after the slower Apple
jobs move off the critical path. Routine cache refreshes remain on standard
capacity.

The v0.10.144 release is the baseline. Its candidate started at 23:01:22Z,
completed at 00:31:15Z, and published at 00:32:37Z, for 91m15s from candidate
start to publication. The Intel Apple job took 75m20s, the ARM Apple job took
53m17s, Windows took 47m05s, and the Linux jobs took 22m46s and 24m40s.

Every comparison below built immutable source
`8d7d82bf191e79de0a0a6319cb863f43d877acce` with benchmark mode. That mode
cannot sign, notarize, package, publish, upload an archive, or save a cache.
Costs apply whole-minute billing to the two target jobs and exclude the
standard-capacity AOT preparation job.

| Receipt | Capacity | Target build times | Full workflow | Projected target cost | Result |
| --- | --- | --- | ---: | ---: | --- |
| [GitHub macOS XLarge](https://github.com/burin-labs/harn/actions/runs/36281705312) | `macos-15-xlarge` | ARM 11m10s; x86 cross 14m36s | 21m46s | $2.96 | both passed |
| [GitHub Linux 16-core](https://github.com/burin-labs/harn/actions/runs/36281767553) | `ubuntu-16core-release` | ARM 8m26s; x86 12m38s | 21m24s | $1.01 | both passed glibc gate |
| [GitHub Linux 32-core](https://github.com/burin-labs/harn/actions/runs/36281754601) | `ubuntu-32core-release` | ARM 8m31s; x86 9m25s | 18m50s | $1.72 | both passed glibc gate |
| [Blacksmith Linux 16-core](https://github.com/burin-labs/harn/actions/runs/36282552096) | `blacksmith-16vcpu-ubuntu-2204` | ARM 6m56s; x86 7m38s | 14m47s | $0.54 | both passed glibc gate |
| [Blacksmith Linux 32-core](https://github.com/burin-labs/harn/actions/runs/36282572802) | `blacksmith-32vcpu-ubuntu-2204` | ARM 7m17s; x86 7m05s | 14m43s | $1.02 | both passed glibc gate |
| [Blacksmith Linux 16-core, Ubuntu 24.04](https://github.com/burin-labs/harn/actions/runs/36281741414) | retired image | ARM 6m54s; x86 7m05s | 12m50s | $0.51 | rejected GLIBC_2.39 |
| [Blacksmith Linux 32-core, Ubuntu 24.04](https://github.com/burin-labs/harn/actions/runs/36281721595) | retired image | ARM 6m26s; x86 7m11s | 14m15s | $1.02 | rejected GLIBC_2.39 |

The 16-core Blacksmith Linux result is faster and cheaper than both GitHub
larger-runner results. The 32-core Blacksmith run saved four seconds of workflow
wall time while nearly doubling the target cost, so the policy selects 16
cores. The Ubuntu 24.04 trials compiled quickly but produced
binaries above Harn's glibc 2.35 compatibility ceiling, so their artifacts were
rejected. Runner operating system, architecture, glibc version, provider, and
rate now live in the policy registry. The matrix resolver rejects a target
whose runner OS or glibc floor is incompatible before dispatch.

The chosen target jobs add about $3.50 per release: $2.96 for both Apple jobs
and $0.54 for both Linux jobs. A measured Blacksmith 16-vCPU AOT job adds about
$0.10, keeping the projected increment below $3.60. The unchanged Windows job
therefore owns the expected critical path at about 47 minutes, with more than
25 minutes of release-tail allowance under the 75-minute objective. Moving
Windows to a follow-up asset would add manifest and consumer complexity without
improving the stated objective, so it remains part of the candidate archive.

Set `HARN_RELEASE_ENABLE_BLACKSMITH_LINUX=true` so candidate and benchmark AOT
preparation uses the policy's Blacksmith runner. The macOS variable is retained
for compatibility, but the selected GitHub XLarge primary does not depend on
it. Explicit `standard` and `fast` benchmark profiles continue to honor the
operator's selected profile.

## Compiler-cache backends

Each target also declares `sccache_backend` next to `use_sccache`:

| Backend | Meaning |
| --- | --- |
| `sticky` | Blacksmith sticky-disk `SCCACHE_DIR`. Hosted runners for the same target fall back to the bounded local blob cache. |
| `local` | Bounded local `SCCACHE_DIR` restored/saved as one Actions cache blob. `SCCACHE_GHA_ENABLED=false`. |
| `install` | Historical Linux install path through `.github/actions/sccache-install`. |
| `none` | Explicit skip with a job-summary reason. |

The per-object GHA sccache backend stays off. Windows previously failed mid
`harn-vm` compile with `os error 10054` (#2114), and the hosted per-object trial
spent 5.35 GiB for negligible hits (v0.10.39). Local blob persistence is the
next-best reusable boundary on hosted macOS ARM and Windows: restore once
before compile, save once after, never talk to the cache API during rustc.

Every Build job records cache mode, hit/miss statistics (or an explicit skip
reason), and Build-step wall time in the job summary. Warm and candidate modes
compare that Build-step duration to
`.github/release-warm-build-budget.json`, which ratchets from the measured
`v0.10.65` fanout ([run 31288092986](https://github.com/burin-labs/harn/actions/runs/31288092986)).
Queue time and skipped targets do not count.

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
