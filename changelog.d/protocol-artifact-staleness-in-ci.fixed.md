Protocol artifacts are now checked for staleness in continuous integration.
`make check-protocol-artifacts` regenerates them and compares, and it joins the
repository policy list that already runs on every pull request, so a
`spec/protocol-artifacts/` file that drifts from its generator is caught before
it reaches integrators rather than only when someone runs the full local
aggregate.
