Hostlib re-exposes legacy `hostlib_*` ambient globals when
`HARN_LEGACY_AMBIENT_CAPABILITIES` is enabled, so ambient pipelines keep
calling the typed capability implementations instead of falling through to
an embedder host bridge.
