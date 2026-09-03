`pattern_learning_context` and `pattern_learning_skill_matches` now accept
`options.task_domain_words`, an optional list of terms the host's own
task-intent or language matcher already decided the current task is
genuinely about. When present, a learned skill only serves its
`<learned_context>` card when at least one of its matched words is a
supplied domain term, so a learned skill whose only lexical overlap with
the task is a generic word no longer serves an irrelevant card. Omitting
the field keeps today's lexical-overlap ranking unchanged.
