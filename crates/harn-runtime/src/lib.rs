//! The complete native Harn runtime.
//!
//! `harn-vm` owns the interpreter host and most builtin families. Other crates
//! own the rest and contribute their builtins and installers through link-time
//! registries. A crate the final binary never references is never linked, and
//! its builtins silently disappear, so every binary that runs or checks Harn
//! source calls [`force_link`] at startup instead of naming each family crate.

/// Register every builtin family on `vm`. Use this rather than
/// `harn_vm::register_vm_stdlib`, which installs only the families `harn-vm`
/// owns unless the binary already linked the rest.
pub fn register_vm_stdlib(vm: &mut harn_vm::Vm) {
    force_link();
    harn_vm::register_vm_stdlib(vm);
}

/// Register the deterministic builtins of every family on `vm`.
pub fn register_core_stdlib(vm: &mut harn_vm::Vm) {
    force_link();
    harn_vm::register_core_stdlib(vm);
}

/// Link every builtin family into the calling binary.
pub fn force_link() {
    harn_vm::stdlib::force_link();
    harn_flow::force_link();
}

#[cfg(test)]
mod tests {
    /// One builtin from each family crate outside `harn-vm`.
    const OWNED_ELSEWHERE: &[&str] = &["flow_invariant_allow"];

    #[test]
    fn every_family_crate_contributes_its_builtins() {
        super::force_link();
        let names: Vec<_> = harn_vm::stdlib::all_builtin_manifest()
            .iter()
            .map(|entry| entry.name)
            .collect();
        for owned_elsewhere in OWNED_ELSEWHERE {
            assert!(
                names.contains(owned_elsewhere),
                "`{owned_elsewhere}` is missing: its family crate was not linked"
            );
        }
    }
}
