- Perf pre-work for the VM-heavier re-architecture (measurement only): recorded
  the pre-migration CLI cold-start baseline in `perf/cli/baselines/main.json`
  (cold + warm medians, Apple Silicon macOS host), added a criterion benchmark
  for the Rust↔`.harn` `harn_entry` boundary crossing (`call_harn_export_typed`
  vs `call_harn_export_by_name`, warm vs cold parent module cache), added a
  transcript-projection benchmark at ~10k/50k/100k-token transcripts, and added
  `perf/README.md` documenting these suites as the regression gates for the
  stage-loop inversion wave.
