`harn install --locked` / `--frozen` / `--offline` no longer fails with
"harn.lock would need to change" after a Harn release bump when the resolved
dependency set is unchanged: the frozen comparison now checks the resolution
content and ignores the `generator_version` / `protocol_artifact_version`
provenance stamps (still refreshed by non-frozen installs and audited by
`harn package audit`). Frozen installs also now correctly fail when the
manifest dropped all dependencies but the lock still pins packages.
