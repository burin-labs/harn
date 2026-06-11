//! macOS sandbox backend — `/usr/bin/sandbox-exec` wrapper rendered
//! from the active capability set.
//!
//! `sandbox-exec` is technically deprecated but remains the platform
//! mechanism most production macOS sandboxes still rely on; Apple has
//! not shipped a supported successor for non-App-Store binaries. The
//! generated profile is a tight default-deny policy with explicit
//! allow rules for the workspace roots and a small list of
//! system-read directories required to exec common binaries.
//!
//! See `docs/src/sandboxing.md` for the capability → kernel-knob
//! mapping table.

use std::path::Path;
use std::process::Command;

use super::{
    policy_allows_network, policy_allows_workspace_write,
    process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_presets, process_sandbox_readonly_roots,
    process_sandbox_roots, unavailable, PrepareOutcome, SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset, SandboxProfile};
use crate::value::VmError;

const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

pub(super) struct Backend;

impl SandboxBackend for Backend {
    fn name() -> &'static str {
        "macos"
    }

    fn available() -> bool {
        Path::new(SANDBOX_EXEC_PATH).exists()
    }

    fn prepare_std_command(
        program: &str,
        args: &[String],
        _command: &mut Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        wrap_with_sandbox_exec(program, args, policy, profile)
    }

    fn prepare_tokio_command(
        program: &str,
        args: &[String],
        _command: &mut tokio::process::Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        wrap_with_sandbox_exec(program, args, policy, profile)
    }
}

fn wrap_with_sandbox_exec(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<PrepareOutcome, VmError> {
    if !Path::new(SANDBOX_EXEC_PATH).exists() {
        return unavailable("macOS sandbox-exec is not available", profile);
    }
    let mut wrapped_args = vec![
        "-p".to_string(),
        render_profile(policy),
        "--".to_string(),
        program.to_string(),
    ];
    wrapped_args.extend(macos_sandbox_compatible_args(program, args));
    Ok(PrepareOutcome::WrappedExec {
        wrapper: SANDBOX_EXEC_PATH.to_string(),
        args: wrapped_args,
    })
}

fn macos_sandbox_compatible_args(program: &str, args: &[String]) -> Vec<String> {
    if is_swiftpm_invocation(program, args) {
        return swiftpm_outer_sandbox_args(args);
    }
    args.to_vec()
}

fn is_swiftpm_invocation(program: &str, args: &[String]) -> bool {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("swift")
        && matches!(
            args.first().map(String::as_str),
            Some("build" | "test" | "run" | "package")
        )
}

fn swiftpm_outer_sandbox_args(args: &[String]) -> Vec<String> {
    let mut rewritten = Vec::with_capacity(args.len() + 9);
    rewritten.push(args[0].clone());
    if !has_swiftpm_option(args, "--disable-sandbox") {
        rewritten.push("--disable-sandbox".to_string());
    }
    if !has_swiftpm_option(args, "--manifest-cache") {
        rewritten.extend(["--manifest-cache".to_string(), "local".to_string()]);
    }
    if !has_swiftpm_option(args, "--cache-path") {
        rewritten.extend([
            "--cache-path".to_string(),
            ".build/harn/swiftpm/cache".to_string(),
        ]);
    }
    if !has_swiftpm_option(args, "--config-path") {
        rewritten.extend([
            "--config-path".to_string(),
            ".build/harn/swiftpm/config".to_string(),
        ]);
    }
    if !has_swiftpm_option(args, "--security-path") {
        rewritten.extend([
            "--security-path".to_string(),
            ".build/harn/swiftpm/security".to_string(),
        ]);
    }
    rewritten.extend(args.iter().skip(1).cloned());
    rewritten
}

fn has_swiftpm_option(args: &[String], option: &str) -> bool {
    let equals_prefix = format!("{option}=");
    args.iter()
        .any(|arg| arg == option || arg.starts_with(&equals_prefix))
}

fn render_profile(policy: &CapabilityPolicy) -> String {
    let developer_toolchain_read_roots = process_sandbox_developer_toolchain_read_roots(policy);
    let package_manager_read_roots = process_sandbox_package_manager_config_read_roots(policy);
    let developer_toolchain_cache_roots =
        super::process_sandbox_developer_toolchain_cache_roots(policy);
    render_profile_with_extra_read_roots(
        policy,
        &developer_toolchain_read_roots,
        &package_manager_read_roots,
        &developer_toolchain_cache_roots,
    )
}

fn render_profile_with_extra_read_roots(
    policy: &CapabilityPolicy,
    developer_toolchain_read_roots: &[std::path::PathBuf],
    package_manager_read_roots: &[std::path::PathBuf],
    developer_toolchain_cache_roots: &[std::path::PathBuf],
) -> String {
    let roots = process_sandbox_roots(policy);
    let read_only_roots = process_sandbox_readonly_roots(policy);
    let policy_read_roots = process_sandbox_policy_read_roots(policy);
    let policy_write_roots = process_sandbox_policy_write_roots(policy);
    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow file-read-metadata)\n\
         (allow file-read-data (literal \"/\"))\n",
    );
    profile.push_str(standard_device_profile_rules());
    for root in preset_read_roots(policy) {
        profile.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            sandbox_profile_escape(root)
        ));
    }
    for root in developer_toolchain_read_roots
        .iter()
        .chain(developer_toolchain_cache_roots.iter())
    {
        profile.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            sandbox_profile_escape(&root.display().to_string())
        ));
    }
    // Process-only read roots and Harn read-only roots are granted read but
    // never write — the write block below iterates only writable roots.
    for root in roots
        .iter()
        .chain(read_only_roots.iter())
        .chain(policy_read_roots.iter())
        .chain(package_manager_read_roots.iter())
    {
        profile.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            sandbox_profile_escape(&root.display().to_string())
        ));
    }
    if policy_allows_workspace_write(policy) {
        for root in preset_write_roots(policy) {
            profile.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                sandbox_profile_escape(root)
            ));
        }
        for root in &policy_write_roots {
            profile.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
        profile.push_str("(allow file-write*");
        for root in preset_write_roots(policy) {
            profile.push_str(&format!(" (subpath \"{}\")", sandbox_profile_escape(root)));
        }
        for root in policy_write_roots
            .iter()
            .chain(developer_toolchain_cache_roots.iter())
        {
            profile.push_str(&format!(
                " (subpath \"{}\")",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
        profile.push_str(")\n");
        for root in roots {
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
        // A read-only root nested under a writable workspace root would
        // otherwise inherit write from the broad allow above —
        // `sandbox-exec` is last-match-wins, so an explicit deny emitted
        // *after* every write allow re-asserts hermetic read-only scope
        // even when the lists are not disjoint. The deny is a no-op for
        // disjoint read-only roots (which never received a write allow).
        for root in read_only_roots
            .iter()
            .chain(package_manager_read_roots.iter())
        {
            profile.push_str(&format!(
                "(deny file-write* (subpath \"{}\"))\n",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
    }
    if policy_allows_network(policy) {
        profile.push_str("(allow network*)\n");
    }
    profile
}

fn preset_read_roots(policy: &CapabilityPolicy) -> Vec<&'static str> {
    let mut roots = Vec::new();
    for preset in process_sandbox_presets(policy) {
        match preset {
            ProcessSandboxPreset::SystemRuntime => roots.extend([
                "/bin",
                "/etc",
                "/Library",
                "/opt/homebrew",
                "/private/etc",
                "/private/var/select",
                "/System",
                "/usr",
                "/var/select",
            ]),
            ProcessSandboxPreset::DeveloperToolchains => roots.extend([
                "/Applications",
                "/Library/Developer",
                "/System/Library/Developer",
            ]),
            ProcessSandboxPreset::PackageManagerConfig => {}
            ProcessSandboxPreset::UserTemp => {}
        }
    }
    roots.sort_unstable();
    roots.dedup();
    roots
}

fn preset_write_roots(policy: &CapabilityPolicy) -> Vec<&'static str> {
    let mut roots = Vec::new();
    if process_sandbox_presets(policy).contains(&ProcessSandboxPreset::UserTemp) {
        roots.extend([
            "/private/tmp",
            "/private/var/folders",
            "/tmp",
            "/var/folders",
            "/var/tmp",
        ]);
    }
    roots
}

fn sandbox_profile_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn standard_device_profile_rules() -> &'static str {
    // Keep this aligned with `is_standard_io_device` in `mod.rs`, but include
    // common read-only entropy/zero devices that language runtimes and test
    // harnesses legitimately open while still avoiding a broad `/dev` grant.
    "(allow file-read* \
       (literal \"/dev/null\") \
       (literal \"/dev/zero\") \
       (literal \"/dev/random\") \
       (literal \"/dev/urandom\") \
       (literal \"/dev/stdin\") \
       (literal \"/dev/stdout\") \
       (literal \"/dev/stderr\") \
       (subpath \"/dev/fd\"))\n\
     (allow file-write* \
       (literal \"/dev/null\") \
       (literal \"/dev/stdout\") \
       (literal \"/dev/stderr\") \
       (subpath \"/dev/fd\"))\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn macos_policy_with_workspace_ops(ops: &[&str]) -> CapabilityPolicy {
        CapabilityPolicy {
            tools: Vec::new(),
            capabilities: std::collections::BTreeMap::from([(
                "workspace".to_string(),
                ops.iter().map(|op| op.to_string()).collect(),
            )]),
            workspace_roots: vec!["/tmp/harn-workspace".to_string()],
            read_only_roots: Vec::new(),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: None,
            tool_arg_constraints: Vec::new(),
            tool_annotations: std::collections::BTreeMap::new(),
            sandbox_profile: SandboxProfile::Worktree,
            process_sandbox: Default::default(),
        }
    }

    #[test]
    fn sandbox_profile_does_not_grant_global_file_read() {
        let profile = render_profile(&macos_policy_with_workspace_ops(&["read_text"]));
        assert!(
            !profile.contains("(allow file-read*)\n"),
            "profile must not grant global file reads"
        );
        assert!(
            profile.contains("(allow file-read-data (literal \"/\"))"),
            "profile should permit root-directory reads needed to exec common macOS binaries"
        );
        assert!(
            profile.contains("harn-workspace"),
            "workspace root should be included in scoped read grants: {profile}"
        );
    }

    #[test]
    fn sandbox_profile_allows_tmp_write_only_with_workspace_write() {
        let read_only = render_profile(&macos_policy_with_workspace_ops(&["read_text"]));
        assert!(
            !read_only.contains("(allow file-write* (subpath \"/tmp\")"),
            "read-only profile must not grant temp writes"
        );

        let writable = render_profile(&macos_policy_with_workspace_ops(&["write_text"]));
        assert!(
            writable.contains("(allow file-write*") && writable.contains("(subpath \"/tmp\")"),
            "writable profile should grant temp writes: {writable}"
        );
        assert!(
            writable.contains("(allow file-read* (subpath \"/private/var/folders\"))"),
            "writable profile should let developer tools read per-user temp caches: {writable}"
        );
        assert!(
            writable.contains("(allow file-write*")
                && writable.contains("(subpath \"/private/var/folders\")"),
            "writable profile should let developer tools update per-user temp caches: {writable}"
        );
    }

    #[test]
    fn sandbox_profile_allows_applications_for_xcode_toolchains() {
        let profile = render_profile(&macos_policy_with_workspace_ops(&["read_text"]));
        assert!(
            profile.contains("(allow file-read* (subpath \"/Applications\"))"),
            "Applications should be readable so Xcode toolchain bundles can load: {profile}"
        );
    }

    #[test]
    fn sandbox_profile_honors_explicit_process_presets() {
        let mut policy = macos_policy_with_workspace_ops(&["write_text"]);
        policy.process_sandbox.presets = Some(vec![ProcessSandboxPreset::SystemRuntime]);
        let profile = render_profile(&policy);

        assert!(
            !profile.contains("(allow file-read* (subpath \"/Applications\"))"),
            "disabling developer_toolchains should remove Xcode bundle access: {profile}"
        );
        assert!(
            !profile.contains("(subpath \"/private/var/folders\")"),
            "disabling user_temp should remove per-user temp cache access: {profile}"
        );
    }

    #[test]
    fn sandbox_profile_allows_package_manager_config_read_only() {
        let temp_home = tempfile::tempdir().expect("temp home");
        std::fs::write(
            temp_home.path().join(".npmrc"),
            "registry=https://registry.example\n",
        )
        .expect("write npmrc");

        let package_roots =
            super::super::package_manager_config_read_roots_for_home(temp_home.path());
        let profile = render_profile_with_extra_read_roots(
            &macos_policy_with_workspace_ops(&["read_text", "write_text", "delete"]),
            &[],
            &package_roots,
            &[],
        );
        let npmrc_path = super::super::package_manager_config_read_roots_for_home(temp_home.path())
            .into_iter()
            .find(|path| path.ends_with(".npmrc"))
            .expect("npmrc root")
            .display()
            .to_string();
        let escaped = sandbox_profile_escape(&npmrc_path);

        assert!(
            profile.contains(&format!("(allow file-read* (subpath \"{escaped}\"))")),
            "package manager config should be readable: {profile}"
        );
        assert!(
            profile.contains(&format!("(deny file-write* (subpath \"{escaped}\"))")),
            "package manager config should be explicitly re-denied writes: {profile}"
        );
        assert!(
            !profile.contains(&format!("(allow file-write* (subpath \"{escaped}\"))")),
            "package manager config must not get direct write access: {profile}"
        );
    }

    #[test]
    fn sandbox_profile_allows_home_toolchain_roots_read_only() {
        let temp_home = tempfile::tempdir().expect("temp home");
        let toolchain_roots =
            super::super::developer_toolchain_read_roots_for_home(temp_home.path());
        let uv_path = toolchain_roots
            .iter()
            .find(|path| path.ends_with(std::path::Path::new(".local/share/uv")))
            .expect("uv root")
            .display()
            .to_string();
        let rustup_path = toolchain_roots
            .iter()
            .find(|path| path.ends_with(std::path::Path::new(".rustup")))
            .expect("rustup root")
            .display()
            .to_string();
        let profile = render_profile_with_extra_read_roots(
            &macos_policy_with_workspace_ops(&["read_text", "write_text", "delete"]),
            &toolchain_roots,
            &[],
            &[],
        );

        for path in [uv_path, rustup_path] {
            let escaped = sandbox_profile_escape(&path);
            assert!(
                profile.contains(&format!("(allow file-read* (subpath \"{escaped}\"))")),
                "home toolchain root should be readable: {profile}"
            );
            assert!(
                !profile.contains(&format!("(allow file-write* (subpath \"{escaped}\"))")),
                "home toolchain root must stay read-only: {profile}"
            );
        }
    }

    #[test]
    fn sandbox_profile_allows_jvm_ios_toolchain_caches_read_write() {
        let temp_home = tempfile::tempdir().expect("temp home");
        let cache_roots =
            super::super::developer_toolchain_cache_write_roots_for_home(temp_home.path());
        let gradle_path = cache_roots
            .iter()
            .find(|path| path.ends_with(std::path::Path::new(".gradle")))
            .expect("gradle cache root")
            .display()
            .to_string();
        let derived_data_path = cache_roots
            .iter()
            .find(|path| {
                path.ends_with(std::path::Path::new("Library/Developer/Xcode/DerivedData"))
            })
            .expect("DerivedData cache root")
            .display()
            .to_string();

        // Writable policy: the build can read AND write its toolchain caches.
        // The writable roots all land on the single `(allow file-write* ...)`
        // line, so isolate that line and assert each cache subpath is on it.
        let writable = render_profile_with_extra_read_roots(
            &macos_policy_with_workspace_ops(&["read_text", "write_text", "delete"]),
            &[],
            &[],
            &cache_roots,
        );
        let write_line = writable
            .lines()
            .find(|line| line.starts_with("(allow file-write* (subpath"))
            .expect("a multi-subpath file-write* allow line");
        for path in [&gradle_path, &derived_data_path] {
            let escaped = sandbox_profile_escape(path);
            assert!(
                writable.contains(&format!("(allow file-read* (subpath \"{escaped}\"))")),
                "JVM/iOS toolchain cache should be readable: {writable}"
            );
            assert!(
                write_line.contains(&format!("(subpath \"{escaped}\")")),
                "JVM/iOS toolchain cache should be writable under a writable policy: {writable}"
            );
        }

        // Read-only policy: caches are readable (dependency resolution) but
        // never writable — the whole workspace-write block is skipped.
        let read_only = render_profile_with_extra_read_roots(
            &macos_policy_with_workspace_ops(&["read_text"]),
            &[],
            &[],
            &cache_roots,
        );
        let gradle_escaped = sandbox_profile_escape(&gradle_path);
        assert!(
            read_only.contains(&format!(
                "(allow file-read* (subpath \"{gradle_escaped}\"))"
            )),
            "toolchain cache should still be readable under a read-only policy: {read_only}"
        );
        assert!(
            !read_only
                .lines()
                .any(|line| line.starts_with("(allow file-write* (subpath")),
            "read-only policy must not emit a workspace file-write allow: {read_only}"
        );
    }

    #[test]
    fn sandbox_profile_honors_process_only_roots() {
        let mut policy = macos_policy_with_workspace_ops(&["write_text"]);
        policy.process_sandbox.read_roots = vec!["/opt/vendor-sdk".to_string()];
        policy.process_sandbox.write_roots = vec!["/opt/vendor-cache".to_string()];
        let profile = render_profile(&policy);

        assert!(
            profile.contains("(allow file-read* (subpath \"/opt/vendor-sdk\"))"),
            "process read roots should be readable: {profile}"
        );
        assert!(
            profile.contains("(allow file-read* (subpath \"/opt/vendor-cache\"))"),
            "process write roots should also be readable: {profile}"
        );
        assert!(
            profile.contains("(subpath \"/opt/vendor-cache\")"),
            "process write roots should be writable when workspace writes are allowed: {profile}"
        );
        assert!(
            !profile.contains("(allow file-write* (subpath \"/opt/vendor-sdk\"))"),
            "process read roots must not be writable: {profile}"
        );
    }

    #[test]
    fn sandbox_profile_allows_standard_devices_without_broad_dev_write() {
        let profile = render_profile(&macos_policy_with_workspace_ops(&["read_text"]));
        for device in ["/dev/null", "/dev/stdin", "/dev/stdout", "/dev/stderr"] {
            assert!(
                profile.contains(&format!("(literal \"{device}\")")),
                "standard device {device} should be explicitly allowed: {profile}"
            );
        }
        for device in ["/dev/zero", "/dev/random", "/dev/urandom"] {
            assert!(
                profile.contains(&format!("(literal \"{device}\")")),
                "common read-only device {device} should be explicitly allowed: {profile}"
            );
        }
        assert!(
            profile.contains("(subpath \"/dev/fd\")"),
            "numeric descriptor aliases should be allowed narrowly: {profile}"
        );
        assert!(
            !profile.contains("(allow file-write* (subpath \"/dev\"))"),
            "profile must not grant broad writes to every device: {profile}"
        );
    }

    #[test]
    fn sandbox_exec_profile_allows_common_device_runtime_access() {
        if !Path::new(SANDBOX_EXEC_PATH).exists() {
            return;
        }
        let profile = render_profile(&macos_policy_with_workspace_ops(&["read_text"]));
        let output = Command::new(SANDBOX_EXEC_PATH)
            .args([
                "-p",
                &profile,
                "--",
                "/bin/sh",
                "-c",
                "cat /dev/null >/dev/null \
                 && dd if=/dev/zero of=/dev/null bs=1 count=1 2>/dev/null \
                 && dd if=/dev/urandom of=/dev/null bs=1 count=1 2>/dev/null",
            ])
            .output()
            .expect("run sandbox-exec device smoke");
        assert!(
            output.status.success(),
            "standard device smoke should pass\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn sandbox_exec_profile_allows_xcrun_to_resolve_swift() {
        if !Path::new(SANDBOX_EXEC_PATH).exists() || !Path::new("/usr/bin/xcrun").exists() {
            return;
        }
        let profile = render_profile(&macos_policy_with_workspace_ops(&["write_text"]));
        let output = Command::new(SANDBOX_EXEC_PATH)
            .args(["-p", &profile, "--", "/usr/bin/xcrun", "--find", "swift"])
            .output()
            .expect("run sandbox-exec xcrun smoke");
        assert!(
            output.status.success(),
            "xcrun should resolve swift inside the sandbox\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("file system sandbox blocked open"),
            "xcrun stderr must not expose a raw sandbox profile miss"
        );
    }

    #[test]
    fn swiftpm_invocations_use_outer_harn_sandbox() {
        let args = strings(["test", "--filter", "SysMonCoreTests"]);
        let rewritten = macos_sandbox_compatible_args("swift", &args);

        assert_eq!(rewritten.first().map(String::as_str), Some("test"));
        assert!(rewritten.iter().any(|arg| arg == "--disable-sandbox"));
        assert!(has_adjacent(&rewritten, "--manifest-cache", "local"));
        assert!(has_adjacent(
            &rewritten,
            "--cache-path",
            ".build/harn/swiftpm/cache"
        ));
        assert!(has_adjacent(
            &rewritten,
            "--config-path",
            ".build/harn/swiftpm/config"
        ));
        assert!(has_adjacent(
            &rewritten,
            "--security-path",
            ".build/harn/swiftpm/security"
        ));
        assert!(has_adjacent(&rewritten, "--filter", "SysMonCoreTests"));

        let explicit = strings([
            "test",
            "--disable-sandbox",
            "--manifest-cache=none",
            "--cache-path",
            ".cache",
        ]);
        let rewritten = macos_sandbox_compatible_args("/usr/bin/swift", &explicit);
        assert_eq!(
            rewritten
                .iter()
                .filter(|arg| *arg == "--disable-sandbox")
                .count(),
            1
        );
        assert!(
            !has_adjacent(&rewritten, "--manifest-cache", "local"),
            "explicit SwiftPM cache policy must not be overwritten: {rewritten:?}"
        );
        assert!(
            !has_adjacent(&rewritten, "--cache-path", ".build/harn/swiftpm/cache"),
            "explicit SwiftPM cache path must not be overwritten: {rewritten:?}"
        );
    }

    #[test]
    fn sandbox_exec_profile_allows_swiftpm_test_to_use_outer_sandbox() {
        if !Path::new(SANDBOX_EXEC_PATH).exists() || !Path::new("/usr/bin/swift").exists() {
            return;
        }
        let temp = tempfile::TempDir::new().expect("temp Swift package");
        write_swift_package(temp.path());
        let policy = macos_policy_with_workspace_ops(&["write_text"]);
        let args = strings(["test", "--filter", "SysMonCoreTests"]);
        let PrepareOutcome::WrappedExec {
            wrapper,
            args: wrapped_args,
        } = wrap_with_sandbox_exec("swift", &args, &policy, SandboxProfile::Worktree)
            .expect("wrap swift test")
        else {
            panic!("macOS backend should wrap with sandbox-exec");
        };

        let output = Command::new(wrapper)
            .args(wrapped_args)
            .current_dir(temp.path())
            .output()
            .expect("run sandboxed swift test smoke");

        assert!(
            output.status.success(),
            "swift test should run inside Harn's outer sandbox\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("sandbox_apply")
                && !stderr.contains("file system sandbox blocked open"),
            "SwiftPM should not expose nested or missing sandbox policy errors: {stderr}"
        );
    }

    #[test]
    fn read_only_roots_are_granted_read_but_never_write() {
        let mut policy = macos_policy_with_workspace_ops(&["write_text"]);
        policy.read_only_roots = vec!["/mnt/memory".to_string()];
        let profile = render_profile(&policy);

        assert!(
            profile.contains("(allow file-read* (subpath \"/mnt/memory\"))"),
            "read-only root should be granted read: {profile}"
        );
        assert!(
            !profile.contains("(allow file-write* (subpath \"/mnt/memory\"))"),
            "read-only root must never be granted write even when workspace_write is allowed: {profile}"
        );
        assert!(
            profile
                .lines()
                .any(|line| line.starts_with("(allow file-write*")
                    && line.contains("harn-workspace")),
            "writable workspace root should still get write: {profile}"
        );
    }

    #[test]
    fn nested_read_only_root_is_denied_write_after_broad_workspace_allow() {
        // /ws/vendor is a read-only root nested under the writable /ws.
        let mut policy = macos_policy_with_workspace_ops(&["write_text"]);
        policy.workspace_roots = vec!["/ws".to_string()];
        policy.read_only_roots = vec!["/ws/vendor".to_string()];
        let profile = render_profile(&policy);

        // /ws/vendor is readable.
        assert!(
            profile.contains("(allow file-read* (subpath \"/ws/vendor\"))"),
            "nested read-only root should be granted read: {profile}"
        );
        // /ws is writable.
        assert!(
            profile.contains("(allow file-write* (subpath \"/ws\"))"),
            "writable workspace root should still get write: {profile}"
        );
        // The broad /ws write allow must be neutralized for /ws/vendor by a
        // deny emitted after it (sandbox-exec is last-match-wins).
        let write_allow = profile
            .lines()
            .position(|line| line == "(allow file-write* (subpath \"/ws\"))")
            .expect("expected a write allow for /ws");
        let vendor_deny = profile
            .lines()
            .position(|line| line == "(deny file-write* (subpath \"/ws/vendor\"))")
            .expect("expected a write deny for /ws/vendor");
        assert!(
            vendor_deny > write_allow,
            "deny for the nested read-only root must come after the broad write allow \
             so last-match-wins keeps it unwritable: {profile}"
        );
    }

    #[test]
    fn read_only_root_deny_is_omitted_when_no_workspace_write() {
        // No write capability: there is no broad write allow to neutralize,
        // so no deny rule is emitted (pure read-only profile stays minimal).
        let mut policy = macos_policy_with_workspace_ops(&["read_text"]);
        policy.read_only_roots = vec!["/mnt/memory".to_string()];
        let profile = render_profile(&policy);
        assert!(
            !profile.contains("(deny file-write*"),
            "read-only profile must not emit spurious deny rules: {profile}"
        );
    }

    fn strings(values: impl IntoIterator<Item = &'static str>) -> Vec<String> {
        values.into_iter().map(str::to_string).collect()
    }

    fn has_adjacent(values: &[String], first: &str, second: &str) -> bool {
        values
            .windows(2)
            .any(|pair| pair[0] == first && pair[1] == second)
    }

    fn write_swift_package(root: &Path) {
        std::fs::create_dir_all(root.join("Sources/SysMonCore")).expect("create source dir");
        std::fs::create_dir_all(root.join("Tests/SysMonCoreTests")).expect("create test dir");
        std::fs::write(
            root.join("Package.swift"),
            r#"// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "SysMonSmoke",
    products: [
        .library(name: "SysMonCore", targets: ["SysMonCore"]),
    ],
    targets: [
        .target(name: "SysMonCore"),
        .testTarget(name: "SysMonCoreTests", dependencies: ["SysMonCore"]),
    ]
)
"#,
        )
        .expect("write package manifest");
        std::fs::write(
            root.join("Sources/SysMonCore/Sample.swift"),
            r"public enum SysMonSmoke {
    public static func sample() -> Int {
        42
    }
}
",
        )
        .expect("write source");
        std::fs::write(
            root.join("Tests/SysMonCoreTests/SysMonCoreTests.swift"),
            r"import XCTest
@testable import SysMonCore

final class SysMonCoreTests: XCTestCase {
    func testSample() {
        XCTAssertEqual(SysMonSmoke.sample(), 42)
    }
}
",
        )
        .expect("write tests");
    }
}
