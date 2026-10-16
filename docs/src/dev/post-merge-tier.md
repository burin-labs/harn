# Post-merge CI tier

Pull requests and merge groups run the proofs a change needs to merge. The
slowest proof families run on the push to `main` that follows each merge,
which always runs and requires every proof:

- the Windows cross-compile check,
- the macOS deny-warnings build and lint,
- the Linux sandbox tests.

The macOS lane also keeps its own path plan on every event: it runs only when
the change touches macOS-gated process, sandbox, secret-store, or CLI paths,
and `macos-nightly.yml` covers the rest.

Conformance and the Harn source, documentation, and script audits stay on
every pull request that changes Rust or Harn sources. They are how a runtime or
stdlib change proves itself before it lands.

`CI status` accepts skips of the post-merge families and nothing else. A failed
or cancelled job, or a skip of any other required proof, still fails it. Each
run that defers them shows a `Post-merge tier` notice.

## When a pull request runs everything

A pull request or merge group runs the post-merge families before it lands
when its change touches a `full_suite` path in the `changes` job of
`.github/workflows/ci.yml`. Those paths cover CI wiring, `Cargo.lock`, the
toolchain pin, the protocol artifacts, and the safety domain: sandbox,
secrets, egress, permissions, consent and credentials.

To ask for everything on any other pull request, add this line to its body
before you push. A merge group cannot read the body, so a declared full suite
runs on the pull request:

```text
CI-Scope: full
```

`full` is the only declarable scope. Any other name fails `Detect changes`.

## What it does not do

- A merge-group run is not a release proof for the post-merge families. The
  release gate reuses a merge-group proof only when every proof job
  succeeded, so it runs the deferred lanes itself.
- A push to `main` no longer reuses its merge group's result. Every push runs
  the full suite on the commit it landed.

`check-ci-cache-policy` enforces this shape: the `changes` job owns the
decision, each deferred family gates on it, and `CI status` excuses only
those skips.
