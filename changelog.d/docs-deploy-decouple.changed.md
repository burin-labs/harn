- **Docs deploys are decoupled from the rest of CI.** The `deploy-docs` job now gates on the
  `docs-site` build succeeding instead of the whole `ci-status` graph, so a docs/site change
  publishes to harnlang.com whenever the site builds green — independent of unrelated backend or
  test lanes. Previously a docs change that merged while `main` was red elsewhere (e.g. a flaky
  Windows test) had its Render deploy silently skipped and never re-fired. Added a `website/README.md`
  documenting the site's stack, build, and deploy flow.
