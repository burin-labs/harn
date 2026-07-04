---
name: scoped
short: Path-scoped skill for infra edits
description: Applies when touching infrastructure files
when-to-use: when editing infra or Dockerfiles
paths:
  - infra/**
  - Dockerfile
---
# Scoped

A path-scoped project skill. `paths` gates host-side hints, not discovery.
