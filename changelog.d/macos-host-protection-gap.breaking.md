A macOS refusal from a host that fails the environment-protection prerequisite now carries a typed
`host_protection_gap`; `harness.system.sandbox_confinement()` declares it as `host_protection_gap: string?`.

Migration: Rust code that builds `SandboxMechanismUnavailable` with a struct literal must add
`host_protection_gap: None`, or construct it with `SandboxMechanismUnavailable::new` or
`SandboxMechanismUnavailable::host_protection_unavailable`.
