Embedders can install a `harn_vm::process_sandbox::SpawnBroker` that runs one
exact argument-list spawn outside the active process confinement, with an
explicit cwd and complete environment. Shells are never brokered, the
catastrophic-command floor still runs first, and the command receipt reports
`sandbox.enforced: false` with `sandbox.broker`.
