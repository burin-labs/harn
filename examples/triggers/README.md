# Trigger manifest examples

These examples show ready-to-customize `[[triggers]]` shapes. Each directory
contains `harn.toml`, `lib.harn`, `README.md`, and `SKILL.md` unless noted:

- `cron-daily-digest/`: cron schedule + local handler
- `scheduled-eval-suite/`: cron schedule + direct eval-pack handler
- `eval-suite-regression-notifier/`: scheduled eval run + regression gate + Slack-on-flip notification
- `context-maintenance/`: lifecycle hooks for non-blocking context refresh and crystallization
- `github-new-issue/`: webhook trigger + local predicate + local handler
- `a2a-reviewer-fanout/`: a2a-push trigger + remote A2A handler
- `local-a2a-dispatch/`: one handler with local and A2A trigger manifests
- `stream-fan-in/`: multi-source handler combining cron and Kafka stream bindings
- `github-stale-pr-nudger/`: scheduled stale pull-request follow-up
- `github-release-notes-generator/`: release note generation from GitHub events
- `slack-keyword-router/`: Slack message routing by simple keyword classes
- `slack-reaction-action/`: action on Slack `reaction_added`
- `slack-thread-summarizer/`: Slack summarize-request predicate + handler
- `linear-sla-breach/`: scheduled Linear SLA scan
- `linear-cycle-planning/`: Linear issue webhook planning intake
- `linear-stuck-issue-bumper/`: scheduled Linear review queue follow-up
- `notion-content-review-scheduler/`: scheduled Notion content review
- `notion-database-watcher/`: Notion poll trigger with durable state key
- `webhook-generic-hmac/`: generic HMAC-verified webhook routing

Validate the full library from the repo root:

```sh
make check-trigger-examples
```
