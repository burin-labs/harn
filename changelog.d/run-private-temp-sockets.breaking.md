Hosts can grant Unix-domain sockets in one Harn execution's private temporary
directory. Concurrent executions in the same workspace receive separate socket
roots, while their own child processes share one root. Missing execution
ownership refuses command preparation instead of using shared temporary storage.

Migration: full Rust `ProcessSandboxPolicy` struct literals must add
`allow_run_temp_sockets: false`, or use `..ProcessSandboxPolicy::default()`.
Hosts requesting private sockets set it to `true` and enter one existing Harn
execution scope around direct operations outside the VM. Linux and permanent
whole-process confinement refuse this new capability.
