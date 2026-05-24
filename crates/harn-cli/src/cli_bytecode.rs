//! AOT-compiled bytecode for embedded CLI scripts (G7 / harn#2300).
//!
//! The build script (`build.rs`) walks [`harn_stdlib::STDLIB_CLI_SCRIPTS`]
//! at compile time, calls `harn_vm::compile_source` on each script, and
//! emits a cache-format `.harnbc` artifact under `$OUT_DIR/cli-bytecode/`.
//! The generated table is `include!`d here so each entry is a static
//! `&[u8]` baked into the binary — no I/O, no allocation at lookup time.
//!
//! ## Why this lives in `harn-cli` and not `harn-stdlib`
//!
//! The spec for G7 sketches the table living in `harn-stdlib`, but
//! `harn-vm` (which owns the compiler) already depends on `harn-stdlib`,
//! so a build-time dep from `harn-stdlib` to `harn-vm` would cycle. The
//! split-gen-crate workaround (Option A in the issue body) trades one
//! cycle for an extra workspace member with no real upside since
//! `harn-cli` is the sole consumer of the lookup. Embedding here keeps
//! the workspace tree flat and the wiring under one crate.
//!
//! ## How dispatch consumes the table
//!
//! See [`crate::dispatch::run_embedded_script`]. When AOT is enabled
//! (the default) and an entry is registered for the dispatched script,
//! the wedge writes the bytecode bytes adjacent to its source tempfile
//! as `<stem>.harnbc`. The existing runtime loader prefers an adjacent
//! artifact over the shared cache, so the parse + typecheck + compile
//! phases are skipped. On any mismatch (e.g. the user toggled
//! `HARN_DISABLE_OPTIMIZATIONS` between build and run), the cache
//! header rejects the artifact and the loader falls back to source — no
//! crash, no special handling.

include!(concat!(env!("OUT_DIR"), "/cli_bytecode_table.rs"));

/// Look up the precompiled bytecode for an embedded CLI script by its
/// registered name (the same name passed to
/// [`harn_stdlib::find_cli_script`]). Returns `None` when no AOT
/// artifact was emitted at build time, in which case dispatch falls
/// back to source compilation.
pub(crate) fn find_cli_script_bytecode(name: &str) -> Option<&'static [u8]> {
    STDLIB_CLI_SCRIPT_BYTECODE
        .iter()
        .find_map(|(entry_name, bytes)| (*entry_name == name).then_some(*bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every script registered in `harn-stdlib` should have an AOT
    /// artifact emitted unless `HARN_SKIP_AOT_CLI_BUILD` was set during
    /// `cargo build` (e.g. in a hermetic CI environment). Treat the
    /// skip as test-skipped rather than failed.
    #[test]
    fn every_cli_script_has_non_empty_bytecode_when_aot_enabled() {
        if STDLIB_CLI_SCRIPT_BYTECODE.is_empty() {
            // Build was run with HARN_SKIP_AOT_CLI_BUILD=1; nothing to
            // verify here.
            return;
        }
        for script in harn_stdlib::STDLIB_CLI_SCRIPTS {
            let bytes = find_cli_script_bytecode(script.name).unwrap_or_else(|| {
                panic!(
                    "no AOT bytecode for CLI script `{}` despite AOT being enabled",
                    script.name
                )
            });
            assert!(
                !bytes.is_empty(),
                "AOT bytecode for `{}` is empty",
                script.name
            );
        }
    }

    #[test]
    fn find_cli_script_bytecode_returns_none_for_unknown_name() {
        assert!(find_cli_script_bytecode("definitely-not-a-real-script").is_none());
    }

    /// Sanity-check the on-disk header — every emitted artifact should
    /// open with the cache magic. This catches accidental empty writes
    /// and table/file mismatches in the generated `include_bytes!`.
    #[test]
    fn every_emitted_artifact_starts_with_cache_magic() {
        if STDLIB_CLI_SCRIPT_BYTECODE.is_empty() {
            return;
        }
        for (name, bytes) in STDLIB_CLI_SCRIPT_BYTECODE {
            assert!(
                bytes.len() >= harn_vm::bytecode_cache::MAGIC.len(),
                "AOT bytecode for `{name}` is shorter than the cache magic"
            );
            assert_eq!(
                &bytes[..harn_vm::bytecode_cache::MAGIC.len()],
                harn_vm::bytecode_cache::MAGIC,
                "AOT bytecode for `{name}` is missing the HARNBC magic"
            );
        }
    }
}
