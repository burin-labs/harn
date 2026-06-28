- The default sandbox `NetworkPolicy` is now deny-all instead of unrestricted: a `SandboxSpec` constructed
  without an explicit network policy gets no egress, so embedders are secure-by-default and must opt into network
  access with a host allowlist (or `NetworkPolicy::Unrestricted`). The wire variants are unchanged, and the
  `harn-serve` permission lowering already denied egress for an empty allowlist; this aligns the type's default
  with that posture.
