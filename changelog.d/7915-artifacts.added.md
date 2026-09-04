- **A run that did the wrong thing successfully can now be told from a run that
  did the work (#7915).** The abandonment rule added earlier convicts a run
  whose tool calls all failed, but a model that calls tools successfully and
  then writes the wrong file leaves no in-run signal at all: the tally is silent
  by construction, and the terminal said `natural` either way. `agent_loop`
  accepts `require_artifacts`, a list of paths the run is supposed to leave
  behind. A declared path that does not exist when the run reaches its own end
  seals `completion_unverified` / lifecycle `failed` with cause
  `declared_artifact_missing`, deterministically and with no model call. The
  contract checks existence only; contents remain the judge's or the caller's
  own verification step. The audit rides on `AgentResult.declared_artifacts`
  for **every** run that declared one, satisfied runs included, so a contract
  that passed can never be confused with one that was skipped. The probe is
  `harness.fs.status`, not `harness.fs.exists`: `exists` collapses a capability
  policy's scope denial into `false` by design, so an audit built on it would
  convict a run for a path it was never permitted to look at. A denied,
  read-only-denied, unrecognized or errored status is reported as
  `unverifiable` and never convicts.
  Callers who declare nothing are unaffected: no path is read and no field is
  added.
