Keep ambient capability-method lookup unique after the CLI installs the
process builtin manifest, so `harn check` with
`HARN_LEGACY_AMBIENT_CAPABILITIES` still resolves names like `store_set`.
