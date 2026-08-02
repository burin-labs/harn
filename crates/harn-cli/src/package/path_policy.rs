/// Whether a directory name is reserved for Harn-owned runtime state.
///
/// Package walkers must apply this predicate only to directories. Files such
/// as `.harn-version` are package metadata and remain visible.
pub(crate) fn is_harn_internal_directory_name(name: &str) -> bool {
    name == ".harn" || name.starts_with(".harn-")
}

#[cfg(test)]
mod tests {
    use super::is_harn_internal_directory_name;

    #[test]
    fn internal_directory_names_have_one_closed_policy() {
        for name in [".harn", ".harn-runs", ".harn-policy"] {
            assert!(is_harn_internal_directory_name(name), "{name}");
        }
        for name in ["harn", ".git", "target", "node_modules"] {
            assert!(!is_harn_internal_directory_name(name), "{name}");
        }
    }
}
