- **Release binary publishing now treats Actions run-artifact uploads as
  best-effort.** Official GitHub Release archives still publish through the
  hard `gh release upload` path, so transient artifact-store outages no longer
  block otherwise valid signed release artifacts before checksum and smoke gates
  can run.
