`harn test --affected-from <git-ref>` now uses the resolved Harn module graph
to run changed modules' transitive importer tests, with a fail-safe full-suite
fallback for unmodelled changes.
