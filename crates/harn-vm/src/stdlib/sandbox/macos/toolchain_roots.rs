//! Program-adjacent toolchain roots admitted by the macOS process sandbox.

use std::path::PathBuf;

use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset};

use super::super::normalize_for_policy;
use super::process_sandbox_presets;

/// Return the installed Go root needed by an explicitly selected Go tool.
///
/// Hosted macOS runners install Go below a runner-owned tool cache rather than
/// a system or per-user toolchain directory. Granting only the executable lets
/// it start, but hides `GOROOT/src`; Go then misreports standard packages as
/// missing. Recognize the official `GOROOT/bin/{go,gofmt}` layout and grant
/// that one toolchain root when the caller opted into developer toolchains.
/// Arbitrary executables keep file-only authority.
pub(super) fn go_read_root(policy: &CapabilityPolicy, program: &str) -> Option<PathBuf> {
    if !process_sandbox_presets(policy).contains(&ProcessSandboxPreset::DeveloperToolchains) {
        return None;
    }

    let program = std::fs::canonicalize(program).ok()?;
    if !matches!(
        program.file_name().and_then(|name| name.to_str()),
        Some("go" | "gofmt")
    ) {
        return None;
    }
    let root = program.parent().and_then(std::path::Path::parent)?;
    if program
        .parent()
        .and_then(std::path::Path::file_name)
        .and_then(|name| name.to_str())
        != Some("bin")
        || !root.join("src/runtime").is_dir()
        || !root.join("pkg/tool").is_dir()
    {
        return None;
    }

    Some(normalize_for_policy(root))
}

#[cfg(test)]
mod tests {
    use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset};

    use super::super::{render_profile_for_program, sandbox_profile_escape};
    use super::normalize_for_policy;

    #[test]
    fn go_root_is_scoped_to_the_selected_program_and_preset() {
        let temp = tempfile::tempdir().expect("temporary hosted tool cache");
        let root = temp.path().join("go/1.26.6/arm64");
        let bin = root.join("bin");
        std::fs::create_dir_all(root.join("src/runtime")).expect("Go runtime source directory");
        std::fs::create_dir_all(root.join("pkg/tool")).expect("Go tool directory");
        std::fs::create_dir_all(&bin).expect("Go binary directory");
        let go = bin.join("go");
        let unrelated = bin.join("unrelated");
        std::fs::write(&go, "").expect("Go executable fixture");
        std::fs::write(&unrelated, "").expect("unrelated executable fixture");

        let policy = CapabilityPolicy::default();
        let expected = normalize_for_policy(&root);
        let root_rule = format!(
            "(allow file-read* (subpath \"{}\"))",
            sandbox_profile_escape(&expected.display().to_string())
        );
        let profile = render_profile_for_program(&policy, &go.display().to_string());
        assert!(
            profile.contains(&root_rule),
            "selected Go tool must be able to read its GOROOT: {profile}"
        );
        let unrelated_profile =
            render_profile_for_program(&policy, &unrelated.display().to_string());
        assert!(
            !unrelated_profile.contains(&root_rule),
            "an arbitrary executable must not gain sibling-directory reads"
        );

        let mut no_toolchains = policy;
        no_toolchains.process_sandbox.presets = Some(vec![ProcessSandboxPreset::SystemRuntime]);
        let disabled_profile =
            render_profile_for_program(&no_toolchains, &go.display().to_string());
        assert!(
            !disabled_profile.contains(&root_rule),
            "the Go root grant must require the developer-toolchains preset"
        );
    }
}
