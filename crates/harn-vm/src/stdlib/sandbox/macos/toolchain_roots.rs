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

/// Foundation's staging directory for atomic file replacement on the boot
/// volume: `TemporaryItems` in the per-user temp dir.
///
/// SwiftPM writes build files through Foundation's atomic replacement, which
/// stages the new file here and renames it into place; without it
/// `swift build` fails reading its own `output-file-map.json`. Foundation
/// takes the directory from `confstr`, not `TMPDIR`, so it cannot be moved
/// into the session. The rest of the per-user temp dir stays ungranted.
pub(super) fn foundation_replacement_root() -> Option<PathBuf> {
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: the buffer is writable for its full length, which is passed.
    let written = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if written == 0 || written > buffer.len() {
        return None;
    }
    buffer.truncate(written - 1);
    let temp = PathBuf::from(String::from_utf8(buffer).ok()?);
    temp.is_absolute()
        .then(|| normalize_for_policy(&temp.join("TemporaryItems")))
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

    /// swiftc through the `/usr/bin` shim, on the product path: xcrun keeps
    /// its lookup cache and clang its module cache in the per-user temp and
    /// cache dirs, which the profile does not grant. Both must follow the
    /// child's environment into the workspace, or the compile cannot load the
    /// standard library and every shim call reports an `xcrun_db` error.
    #[test]
    fn swiftc_compiles_with_its_caches_in_the_workspace() {
        use crate::stdlib::sandbox::PrepareOutcome;

        if !std::path::Path::new(super::super::SANDBOX_EXEC_PATH).exists()
            || !std::path::Path::new("/usr/bin/swiftc").exists()
        {
            return;
        }
        let workspace = tempfile::tempdir().expect("workspace");
        let root = workspace
            .path()
            .canonicalize()
            .expect("canonical workspace");
        std::fs::write(root.join("hello.swift"), "print(\"hi\")\n").expect("source");
        let policy = CapabilityPolicy {
            sandbox_profile: crate::orchestration::SandboxProfile::Worktree,
            workspace_roots: vec![root.display().to_string()],
            side_effect_level: Some("process_exec".to_string()),
            ..CapabilityPolicy::default()
        };
        let args = ["-o", "hello", "hello.swift"].map(str::to_string);
        let PrepareOutcome::WrappedExec { wrapper, args } = super::super::wrap_with_sandbox_exec(
            "/usr/bin/swiftc",
            &args,
            &policy,
            crate::orchestration::SandboxProfile::Worktree,
        )
        .expect("wrap swiftc") else {
            panic!("macOS backend should wrap with sandbox-exec");
        };
        let mut env = Vec::new();
        crate::stdlib::sandbox::inject_workspace_process_env(&mut env, &policy);
        let output = std::process::Command::new(wrapper)
            .args(args)
            .envs(env)
            .current_dir(&root)
            .output()
            .expect("run sandboxed swiftc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "swiftc must compile: {stderr}");
        assert!(
            root.join("hello").is_file(),
            "swiftc wrote no binary: {stderr}"
        );
        assert!(
            !stderr.contains("xcrun_db"),
            "xcrun cache refused: {stderr}"
        );
    }
}
