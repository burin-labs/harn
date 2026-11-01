Crystallization evidence cards now carry each session's command ledger (bounded, redacted commands with exit codes), and
the curator's `inspect_session_evidence` reads a bounded, redacted transcript from the session store through the new
`pattern_learning_session_transcript`, so it works for sessions that ended in earlier processes.
