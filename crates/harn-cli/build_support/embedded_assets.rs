use std::path::Path;

const EMBEDDED_ASSET_DIRS: &[&str] = &["assets/persona-templates", "portal-dist"];

/// Keep every directory embedded by `include_dir!` under one Cargo-owned watch
/// contract. The production build calls this after the portal fallback exists,
/// so Cargo observes nested portal assets on fresh checkouts and real builds.
pub(crate) fn emit_watches(manifest_dir: &Path) {
    for relative_dir in EMBEDDED_ASSET_DIRS {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(relative_dir).display()
        );
    }
}
