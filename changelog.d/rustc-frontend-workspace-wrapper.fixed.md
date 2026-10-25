The parallel rustc front end now reaches only Harn's own crates, through a
Cargo workspace rustc wrapper. Setting `RUSTC_BOOTSTRAP=1` for the whole
build had switched `anyhow`, `thiserror`, and `proc-macro2` to nightly-only
code in every dependency. Dependencies now compile exactly as on stable,
published release builds run without unstable flags, and Windows builds stay
serial because they cannot run the wrapper.
