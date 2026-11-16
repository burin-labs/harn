Workspace file writes through shell redirection and truncation now follow
ordinary command-write approval and confinement even when Git tracks the file.
The command scanner no longer launches Git probes or treats these writes as
never approvable. Unresolved execution roots, unrelated destructive commands,
sensitive-path policy and process confinement retain their existing guards.
