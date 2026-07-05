- Run the runtime's own registered reminder-provider and session-hook closures
  as trusted bridge calls. Under an active execution policy the agent loop
  previously killed every turn with
  `tool_rejected: bridged builtin '...' exceeds execution policy` the moment a
  registered closure's body invoked a host-provided builtin; the trusted-bridge
  guard is now held across each provider/hook closure invocation so first-party
  closures the runtime chose to fire are no longer mistaken for model-issued
  tool calls. Model-issued bridged builtins remain gated.
