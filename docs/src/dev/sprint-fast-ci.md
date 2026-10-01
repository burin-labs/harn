# Sprint fast CI

During a short integration sprint, pull requests and merge groups can skip the
slowest proof families and leave them to the push to `main` that follows each
merge.

## Turn it on

```bash
gh variable set SPRINT_FAST_CI --body true --repo burin-labs/harn
```

While the variable is exactly `true`, pull requests and merge groups skip:

- the Windows cross-compile check,
- the macOS deny-warnings build and lint,
- Harn conformance and the Harn source, documentation, and script audits,
- the Linux sandbox tests.

`CI status` accepts those skips and nothing else. A failed or cancelled job,
or a skip of any other required proof, still fails it. Each run shows a
`Sprint fast CI` notice naming what was skipped.

Pushes to `main` ignore the variable and run and require every proof, so a
regression the sprint skipped turns `main` red on the merge that introduced
it.

## Turn it off

```bash
gh variable delete SPRINT_FAST_CI --repo burin-labs/harn
```

Any value other than exactly `true` has the same effect. No code change is
needed in either direction.

## What it does not do

- A merge group run in sprint mode is not a release proof. The release gate
  reuses a merge-group proof only when every proof job succeeded, so it runs
  the skipped lanes itself.
- Sprint runs do not count toward the merge-queue latency measurement, which
  admits only runs where every measured job succeeded.

`check-ci-cache-policy` enforces this shape: the `changes` job is the only
reader of the variable, each skipped family gates on its decision, and
`CI status` excuses only those skips.
