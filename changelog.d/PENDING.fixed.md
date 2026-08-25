Runtime feedback about a turn that has already ended no longer persists for the
rest of the session. Corrective directives raised by the post-turn and terminal
callbacks, the malformed-call and missing-call detectors, the background-command
digest, the exhausted-await notice, the structural and step judges, and the
required-tools check are now spent by the turn that reads them, so a single
injection can no longer be re-served on every later turn demanding an action the
model already took. Guidance whose re-injection is capped, such as
`stall_diagnostics`, stays durable and is unaffected.
