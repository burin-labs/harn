- **Edit-tool rewrite router removed.** The agent loop no longer observes
  freeform edit calls to suggest or rewrite them into structural edit tools.
  Gone are the `compass` agent option, the `compass_ast_edits` reminder
  provider, the `compass_routing_decision` agent event, and the
  `harn.compass.*` metric namespace. The `edit_*` stdlib tools are unchanged
  and the model still chooses among them from their descriptions.

  Migration: stop passing `compass` to `agent_loop` or persona manifests; the
  loop no longer reads it. Drop `compass_ast_edits` from
  `reminders: {providers: [...]}`. Hosts that match on the
  `compass_routing_decision` event kind or the generated
  `CompassRoutingDecision` type must drop that case after regenerating their
  protocol bindings.
