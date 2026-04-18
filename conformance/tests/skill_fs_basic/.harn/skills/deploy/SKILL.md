---
name: deploy
description: Deploy the application to production
when-to-use: User says deploy / ship / release
allowed-tools:
  - bash
  - git
paths:
  - infra/**
  - Dockerfile
model: claude-opus-4-7
effort: high
argument-hint: "<target-env>"
---
# Deploy runbook

1. Verify tests are green.
2. Bump the version in `Cargo.toml`.
3. Run `./scripts/release.sh $1`.
4. Watch logs for the next 10 minutes.
