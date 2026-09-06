Workflow graph normalization now preserves live model policies, including session identity and stall settings.
Agent stages retain stall and verification evidence across shared sessions; explicit `stall_checkpoints`
carry those lifetimes into a subsequent workflow while fresh sessions remain independent.
