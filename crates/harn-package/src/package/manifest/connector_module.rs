//! Which source file a package declares as a provider's connector module.
//!
//! The connector ABI pins every runtime export of that module to a root
//! `harness: Harness`. `HARN-LNT-056` needs to know before it may propose
//! narrowing one, and `harn fix` writes that proposal to disk — so the answer
//! has to come from the manifest's declaration rather than from whatever the
//! file's own contents imply (harn#6186).

use std::path::{Path, PathBuf};

use crate::package::manifest_search::nearest_manifest_or_warn;

/// Whether the nearest `harn.toml` names `harn_file` under any
/// `[[providers]] connector = { harn = ... }`.
///
/// Both sides are canonicalized so a relative manifest entry and an absolute
/// walk path compare equal; a path that cannot be canonicalized (it may not
/// exist yet) falls back to itself, which simply fails to match.
#[must_use]
pub fn is_declared_connector_module(harn_file: &Path) -> bool {
    let Some((manifest, dir)) = nearest_manifest_or_warn(harn_file) else {
        return false;
    };
    let target = canonical(harn_file);
    manifest
        .providers
        .iter()
        .filter_map(|provider| provider.connector.harn.as_deref())
        .any(|module| canonical(&dir.join(module)) == target)
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::MANIFEST;
    use std::fs;

    #[test]
    fn connector_module_declaration_is_read_from_the_manifest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"probe-connector\"\nversion = \"0.1.0\"\n\n[[providers]]\nid = \"probe\"\nconnector = { harn = \"src/lib.harn\" }\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        let declared = root.join("src").join("lib.harn");
        let sibling = root.join("src").join("helpers.harn");
        fs::write(&declared, "pub fn provider_id() { return \"probe\" }\n").unwrap();
        fs::write(&sibling, "pub fn helper() { return 1 }\n").unwrap();

        assert!(is_declared_connector_module(&declared));
        assert!(
            !is_declared_connector_module(&sibling),
            "only the module the manifest names is a connector module"
        );
    }

    #[test]
    fn a_file_with_no_manifest_declares_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let orphan = tmp.path().join("lib.harn");
        fs::write(&orphan, "pub fn provider_id() { return \"probe\" }\n").unwrap();

        assert!(
            !is_declared_connector_module(&orphan),
            "no manifest means no declaration, so the lint falls back to inference"
        );
    }
}
