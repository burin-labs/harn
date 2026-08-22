use std::path::PathBuf;

use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset};

use super::{
    developer_toolchain_cache_write_roots_for_home, normalize_for_policy, process_sandbox_presets,
    sandbox_user_home_dir,
};

pub(crate) fn process_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    if !process_sandbox_presets(policy).contains(&ProcessSandboxPreset::DeveloperToolchains) {
        return Vec::new();
    }
    let mut roots = sandbox_user_home_dir()
        .map(|home| developer_toolchain_cache_write_roots_for_home(&home))
        .unwrap_or_default();
    if let Some(cache_root) = existing_package_root(crate::user_dirs::package_cache_dir()) {
        roots.push(cache_root);
    }
    roots.sort_unstable();
    roots.dedup();
    roots
}

/// Return a narrowly grantable package-cache root for a sandboxed child.
/// Missing paths and filesystem roots cannot widen process authority.
fn existing_package_root(cache_root: Option<PathBuf>) -> Option<PathBuf> {
    let root = normalize_for_policy(cache_root?.as_path());
    (root.parent().is_some() && root.is_dir()).then_some(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::ProcessSandboxPolicy;

    #[test]
    fn allows_only_an_existing_non_root_directory() {
        let cache = tempfile::tempdir().expect("custom cache");
        let allowed = existing_package_root(Some(cache.path().to_path_buf()));
        assert_eq!(allowed, Some(cache.path().canonicalize().unwrap()));

        assert_eq!(
            existing_package_root(Some(cache.path().join("missing"))),
            None
        );
        assert_eq!(existing_package_root(Some(PathBuf::from("/"))), None);
        assert_eq!(existing_package_root(None), None);
    }

    #[test]
    fn requires_the_developer_toolchains_authority() {
        let cache = tempfile::tempdir().expect("custom cache");
        let policy = CapabilityPolicy {
            process_sandbox: ProcessSandboxPolicy {
                presets: Some(Vec::new()),
                ..ProcessSandboxPolicy::default()
            },
            ..CapabilityPolicy::default()
        };

        assert!(process_roots(&policy).is_empty());
        assert!(existing_package_root(Some(cache.path().to_path_buf())).is_some());
    }
}
