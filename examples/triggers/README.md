# Trigger manifest examples

These examples show the currently validated `[[triggers]]` shapes:

- `cron-daily-digest/`: cron schedule + local handler
- `github-new-issue/`: webhook trigger + local predicate + local handler
- `a2a-reviewer-fanout/`: a2a-push trigger + remote A2A handler
- `stream-fan-in/`: multi-source handler combining cron and Kafka stream bindings
