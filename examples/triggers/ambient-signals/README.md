# Ambient signals monitor pack

Turns repository and production conditions into ambient signals for the agent
sessions running on this machine:

- default-branch CI is red, or recovered;
- a repository has at least a threshold of open pull requests;
- fewer self-hosted runners in a runner group, carrying the required labels,
  are online than a minimum;
- a GitHub token's REST rate limit is under a percentage;
- Sentry, Grafana, or Honeycomb sent an alert or its resolution.

Edit `MONITORS` in `lib.harn`, export `GITHUB_TOKEN`, give rate-limit entries
GitHub App credentials, then run the pack:

```sh
harn orchestrator serve --config examples/triggers/ambient-signals/harn.toml
```

The GitHub monitors poll every five minutes and need no inbound network
access. A poll that cannot read GitHub reports `unmeasured` and changes no
signal. The alert webhooks need an address the alerting service can reach;
each reminder says whether the delivery's signature was verified.

Signals land in the local inbox (`harn signals list`). Connect agent sessions
with [Deliver signals to Claude Code and Codex](../../../docs/src/signals-hooks.md).
