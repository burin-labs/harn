//! The complete native Harn runtime.
//!
//! `harn-vm` owns the interpreter host and most builtin families. Other crates
//! own the rest and contribute their builtins and installers through link-time
//! registries. A crate the final binary never references is never linked, and
//! its builtins silently disappear, so every binary that runs or checks Harn
//! source calls [`force_link`] at startup instead of naming each family crate.

/// Link every builtin family into the calling binary.
pub fn force_link() {
    harn_vm::stdlib::force_link();
    harn_flow::force_link();
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_family_crate_contributes_its_builtins() {
        super::force_link();
        let names: Vec<_> = harn_vm::stdlib::all_builtin_manifest()
            .iter()
            .map(|entry| entry.name)
            .collect();
        for owned_elsewhere in ["flow_invariant_allow"] {
            assert!(
                names.contains(&owned_elsewhere),
                "`{owned_elsewhere}` is missing: its family crate was not linked"
            );
        }
    }
}
