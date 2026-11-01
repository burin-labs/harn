`crystallization_curate` accepts `installed` (the host's installed capabilities) and never stages a proposal that
matches one by name or verifier line, and `require_new_evidence: true` skips the model call when no new session
evidence was imported.
