`AcpServerConfig` is now `#[non_exhaustive]` and gains the
`launcher_session_environment` field, so later launch inputs can be added
without another breaking release.

Migration: Rust embedders that built `AcpServerConfig` with a struct literal
must construct it with `AcpServerConfig::new(pipeline)` and the `with_*`
builder methods (for example `with_launcher_session_environment`). Reading and
assigning public fields is unchanged.
