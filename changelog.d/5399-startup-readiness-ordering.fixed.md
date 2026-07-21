The orchestrator now announces readiness — both the `/readyz` flag and the
`HTTP listener ready on …` log — only after startup housekeeping is durably
recorded (state snapshot written; `startup` and `startup_stranded_envelopes`
lifecycle events appended), rather than before. Previously readiness was
signalled first, so an observer that reacted to it and then read the event log
could race the durability of those startup events. This fixes the flaky
`restart_surfaces_stranded_envelopes_and_recover_replays_them_explicitly` e2e
test (harn#5399), whose `wait_for_topic_event` helper is now a single
deterministic read: it no longer relies on `subscribe`, whose in-process
broadcast tail cannot observe another process's appends. The listener still
accepts connections as soon as it binds; only the readiness *signal* moves.
