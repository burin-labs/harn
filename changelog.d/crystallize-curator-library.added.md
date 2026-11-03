`crystallization_curate` accepts `installed` (the host's installed capabilities) and never stages a proposal that
matches one by name or verifier line. With `require_new_evidence: true`, it skips the model call when all evidence
was already reviewed or when every unseen session only ran installed checks and passed.
