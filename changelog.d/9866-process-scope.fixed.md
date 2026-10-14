Per-command process workspace scopes now narrow explicit parent roots, and
`harness.process.run` honors the shared scope for both execution and spawning.
A verification job can require OS confinement without granting access to its controller's checkout.
