`scripts/publish.harn` stages `cargo metadata` JSON through a temp file so
crates.io finalize is not truncated by the ~50 KiB hostlib stdout capture
limit (which blocked publishing v0.10.29).
