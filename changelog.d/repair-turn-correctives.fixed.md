Four fixes to the turn an agent loop spends recovering a tool call the model
described but never emitted, found by watching one live run.

The corrective that shows the model the call to send no longer renders argument
values. It used to show a type placeholder per argument (`{"args": {"path":
"<string>"}}`); the model copied that token into the repair call and then into
every call for the rest of the session, filling optional arguments it had never
used before with the same string. Inside a JSON string a placeholder is
indistinguishable from a value. The exemplar is now the bare call envelope
(`{"name": "edit", "args": {}}`) and the arguments the call must carry are named
in prose, from the tool's own declared required parameters.

A request that exhausts its own whole-request deadline is no longer retried
unchanged. The retry policy read `timeout` as one fact and re-sent identical
bytes under an identical deadline, so a slow constrained call burned its whole
attempt budget re-running a race it had already watched end before reaching the
fallback that could have changed the outcome. A stalled provider — an idle or
first-chunk deadline, a 408, a 504, a dropped connection — is still retried.

A repair turn whose completion arrives as a bare `{"name": ..., "args": ...}`
envelope is no longer charged with a protocol violation when the decode grammar
did not survive to the provider. The leniency was keyed on the contract being
applied; a constrained call that times out or is rejected falls back to an
unconstrained one whose completion is still the envelope the repair prompt asked
for. It is now keyed on the turn being claimed for repair.

Every corrective now names the same call syntax the active parser accepts. The
taught shape per tool format moved to one owner (`std/llm/call_shape`) that both
the agent-side prompt bindings and the parse-side recovery notes read, after a
session was observed teaching `<tool_call>` from the unrecognized-span guidance
and a ```tool fence from everything else in the same transcript.
