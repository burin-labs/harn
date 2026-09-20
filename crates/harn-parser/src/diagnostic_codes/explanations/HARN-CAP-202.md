# HARN-CAP-202 — confined host process cannot open the loopback listener a child's egress proxy needs

## What it means

Harn mediates a sandboxed child's network through a small forwarding proxy
that it runs itself, bound to an ephemeral port on `127.0.0.1`. The OS
sandbox then limits the child to those loopback ports, so the proxy is the
only way out and every destination decision passes through Harn's egress
policy.

This diagnostic fires when Harn could not bind that listener because the
operating system refused the bind with a permission error. That is not a
policy decision Harn made. It means the Harn process itself is running
inside a sandbox (its own `harn run` jail, a parent Harn process's jail, or
another confinement such as Seatbelt or Landlock) that does not grant
`network-bind` on loopback. A confined parent cannot open a listener, so it
cannot mediate egress for a child, so the child cannot be started under a
managed network policy.

The message names the protocol whose listener failed, the sandbox profile
the Harn process reports itself under, and the OS error.

## Why it is its own code

The raw OS error reads `Operation not permitted` with no subject. Consumers
diagnosed it by hand as a cache-path problem (a compiler cache dying on
`127.0.0.1:4226` under confinement) and as a sandbox defect in the test
runner. It is one condition with one cause and one remedy, so it carries one
code. `HARN-CAP-201` is the neighbouring case where the *active profile*
denied a harness capability by policy; here the profile is the confined
parent's, and the denial comes from the OS beneath Harn.

Only `PermissionDenied` is classified this way. `AddrInUse`, exhausted
descriptors, and other bind failures keep their unclassified shape, so this
code cannot absorb an unrelated failure.

## How to fix it

Run the Harn process that spawns the child outside the confinement, or grant
that confinement loopback bind. Concretely:

- If the confined Harn process is nested under an outer `harn run`, give
  that outer run `--allow-process-loopback`. It adds a loopback-only bind
  grant to the jail and keeps the external-egress deny.
- If a test file spawns a child with a network policy, run it through a bare
  `harn test` rather than through a wrapper that is itself sandboxed.
- If Harn is nested under a confinement it does not control, move the child
  spawn to the unconfined level, or spawn the child with
  `NetworkPolicy::Unrestricted` so no proxy is needed.

Widening the *child's* profile does not help: the listener is opened by the
parent before the child exists.
