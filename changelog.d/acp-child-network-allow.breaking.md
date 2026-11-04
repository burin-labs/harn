- **Sandboxed sessions can hold children to a host list.** `AcpSandboxConfig::child_network_allow` holds each
  confined child with network authority to one deny-by-default proxy that admits only the listed hosts.
  `ProcessSandboxPolicy::deny_network` gives a child no IP networking at any side-effect level.

  Migration: Rust consumers constructing `AcpSandboxConfig` or `ProcessSandboxPolicy` literals must provide the new
  fields or use `..Default::default()`. `None` and `false` keep today's behavior.
