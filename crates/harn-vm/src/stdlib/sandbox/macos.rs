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
use std::process::{Command, Output};

use super::{
    policy_allows_network, policy_allows_workspace_write,
    process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_presets, process_sandbox_readonly_roots,
    process_sandbox_roots, process_spawn_error, spawn_error, unavailable, PrepareOutcome,
    ProcessCommandConfig, SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset, SandboxProfile};
use crate::value::VmError;

mod toolchain_roots;

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

    fn run_to_output(
        program: &str,
        args: &[String],
        config: &ProcessCommandConfig,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<Output, VmError> {
        let mut command = super::build_std_command::<Self>(program, args, policy, profile)?;
        super::apply_process_config(&mut command, config, Some(policy));
        let output = crate::op_interrupt::capture_output_interruptible(&mut command)
            .map_err(|error| process_spawn_error(&error).unwrap_or_else(|| spawn_error(error)))?;
        match crate::process_sandbox::macos_wrapped_spawn_io_error(
            output.status.code().unwrap_or(-1),
            &output.stderr,
        ) {
            Some(error) => Err(spawn_error(error)),
            None => Ok(output),
        }
    }
}

fn wrap_with_sandbox_exec(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<PrepareOutcome, VmError> {
    if !Path::new(SANDBOX_EXEC_PATH).exists() {
        return unavailable(
            super::SandboxMechanism::MacosSandboxExec,
            super::SandboxMechanismAvailability::AbsentOnHost,
            profile,
        );
    }
    let mut wrapped_args = vec![
        "-p".to_string(),
        render_profile_for_program(policy, program),
        "--".to_string(),
        program.to_string(),
    ];
    wrapped_args.extend(macos_sandbox_compatible_args(program, args));
    Ok(PrepareOutcome::WrappedExec {
        wrapper: SANDBOX_EXEC_PATH.to_string(),
        args: wrapped_args,
    })
}

fn render_profile_for_program(policy: &CapabilityPolicy, program: &str) -> String {
    let mut developer_toolchain_read_roots = process_sandbox_developer_toolchain_read_roots(policy);
    developer_toolchain_read_roots.extend(toolchain_roots::go_read_root(policy, program));
    developer_toolchain_read_roots.sort_unstable();
    developer_toolchain_read_roots.dedup();

    render_profile_with_extra_read_roots(
        policy,
        &developer_toolchain_read_roots,
        &process_sandbox_package_manager_config_read_roots(policy),
        &super::process_sandbox_developer_toolchain_cache_roots(policy),
    )
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

#[cfg(test)]
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
    // Callers may provide roots outside the normal policy-root builders (for
    // example, an isolated toolchain cache in a test or an embedder-owned
    // cache). Normalize them here as well so macOS aliases such as
    // `/var/folders` and `/private/var/folders` cannot make a broad preset
    // allow miss a narrower read-only deny.
    let developer_toolchain_read_roots = normalize_profile_roots(developer_toolchain_read_roots);
    let read_deny_roots = super::process_sandbox_read_deny_roots(policy);
    let package_manager_read_roots = normalize_profile_roots(package_manager_read_roots);
    let developer_toolchain_cache_roots = normalize_profile_roots(developer_toolchain_cache_roots);
    let roots = process_sandbox_roots(policy);
    let read_only_roots = process_sandbox_readonly_roots(policy);
    let policy_read_roots = process_sandbox_policy_read_roots(policy);
    let policy_write_roots = process_sandbox_policy_write_roots(policy);
    // `signal` is its own SBPL operation, so `(deny default)` blocks it even
    // with `(allow process*)`. A script that backgrounds a helper and kills it
    // from a `trap` would silently fail to reap it, and the orphan keeps the
    // parent's stdout pipe open — the caller then hangs on a read that never
    // returns EOF, which reads as "the sandbox made my program hang" rather
    // than as a denial. Signalling is scoped to the sandbox the process is
    // already in, so this grants reach over its own children, not the host.
    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow signal (target same-sandbox))\n\
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
        for root in &roots {
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
        //
        // Exception: a read-only root that coincides with or nests under a
        // developer-toolchain cache-write root is NOT re-denied. A host may
        // list a cache/dependency path (e.g. `~/.cargo/registry`, `~/go/pkg/mod`)
        // read-only so the agent can browse dependency sources; but the
        // toolchain must WRITE that same cache while it builds, and the preset
        // granted it write above. Last-match-wins would otherwise cancel that
        // grant, breaking the build with a misleading toolchain error
        // (`operation not permitted`, or Go's "not in std"). The cache preset's
        // write intent wins for its own roots; a read-only root OUTSIDE every
        // cache root (a vendored dir, a workspace subtree) is still re-denied.
        for root in read_only_roots
            .iter()
            .chain(package_manager_read_roots.iter())
        {
            if developer_toolchain_cache_roots
                .iter()
                .any(|cache| root.starts_with(cache))
            {
                continue;
            }
            for path in sandbox_profile_path_aliases(&root.display().to_string()) {
                profile.push_str(&format!(
                    "(deny file-write* (subpath \"{}\"))\n",
                    sandbox_profile_escape(&path)
                ));
            }
        }
        // A read-only root that *contains* an explicitly granted write root is
        // the mirror of the nesting handled above, and last-match-wins would
        // otherwise let the broad deny revoke the narrow grant. A policy that
        // says "read anywhere, write here" is coherent and common — it is the
        // only way to scope Harn's own file builtins without also deciding
        // where a subprocess may write — so re-assert the specific grant after
        // the broad deny. Grants disjoint from every read-only root re-emit an
        // allow they already had, which is a no-op.
        for root in granted_write_roots(&roots, &policy_write_roots, policy)
            .iter()
            .filter(|root| {
                read_only_roots
                    .iter()
                    .any(|read_only| root.starts_with(read_only))
            })
        {
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
        // Standard process I/O devices are not workspace filesystem mutations
        // (see `check_fs_path_scope`, which exempts them on the Harn side).
        // They are emitted at the top of the profile too; a broad read-only
        // root would otherwise deny `/dev/null` and break any child that opens
        // it, which is a confusing way to learn a read root was too wide.
        profile.push_str(standard_device_profile_rules());
    }
    // An explicit loopback-only grant is narrower than the ambient tool
    // ceiling. ACP code mode may allow network-capable Harn tools, but that
    // must not widen arbitrary child sockets beyond localhost.
    if policy_allows_network(policy) && !policy.process_sandbox.allow_tcp_loopback {
        if let Some(proxy) = policy.process_network_proxy {
            for port in [proxy.http_port, proxy.socks_port] {
                profile.push_str(&format!(
                    "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
                ));
            }
        } else {
            profile.push_str("(allow network*)\n");
        }
    }
    if policy.process_sandbox.allow_tcp_loopback {
        // Local test servers need all three operations: bind/listen, accept,
        // and the client connection back to the listener. Match the remote
        // endpoint on outbound. A `local ip` outbound filter also matches an
        // unbound socket's wildcard source and would silently admit remote
        // egress. Keeping these rules separate from `network*` preserves the
        // managed-proxy boundary when the run also has provider/network work.
        profile.push_str("(allow network-bind (local ip \"localhost:*\"))\n");
        profile.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");
        profile.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
    }
    // The read denylist is emitted LAST, after every allow in this function.
    //
    // `sandbox-exec` is last-match-wins, so position is the enforcement: a deny
    // written here beats the preset allows, the workspace and read-only allows,
    // the policy read roots, and the write block's read re-grants. That ordering
    // is the requirement, not an optimization — `PackageManagerConfig` grants
    // `~/.config`, `~/.cache`, and `~/.netrc` wholesale, so a denial that
    // competed with presets instead of beating them would never fire on the
    // paths it exists for.
    //
    // `file-read*` covers metadata as well as data, so this also narrows the
    // global `(allow file-read-metadata)` at the top of the profile. Without
    // that, a denied path's existence and size would still leak to a child that
    // cannot open it.
    for root in read_deny_roots {
        for path in sandbox_profile_path_aliases(&root.display().to_string()) {
            profile.push_str(&format!(
                "(deny file-read* (subpath \"{}\"))\n",
                sandbox_profile_escape(&path)
            ));
        }
    }
    profile
}

fn normalize_profile_roots(roots: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    roots
        .iter()
        .map(|root| super::normalize_for_policy(root))
        .collect()
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

/// Every path this profile has explicitly granted write access to.
///
/// Used to re-assert those grants after a broader read-only deny. Preset roots
/// are included because a policy that opts into `UserTemp` and then declares a
/// wide read-only root still means for the temp dir to be writable.
fn granted_write_roots(
    workspace_roots: &[std::path::PathBuf],
    policy_write_roots: &[std::path::PathBuf],
    policy: &CapabilityPolicy,
) -> Vec<std::path::PathBuf> {
    let mut granted = workspace_roots.to_vec();
    granted.extend(policy_write_roots.iter().cloned());
    granted.extend(
        preset_write_roots(policy)
            .into_iter()
            .map(std::path::PathBuf::from),
    );
    granted.sort();
    granted.dedup();
    granted
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

/// macOS exposes `/tmp` and parts of `/var` through both logical and
/// `/private` paths. Seatbelt evaluates those spellings independently, so a
/// writable alias can bypass a deny rule emitted for only one spelling.
fn sandbox_profile_path_aliases(path: &str) -> Vec<String> {
    let mut aliases = vec![path.to_string()];
    if path == "/tmp" || path.starts_with("/tmp/") || path == "/var" || path.starts_with("/var/") {
        aliases.push(format!("/private{path}"));
    } else if path == "/private/tmp"
        || path.starts_with("/private/tmp/")
        || path == "/private/var"
        || path.starts_with("/private/var/")
    {
        aliases.push(path.replacen("/private", "", 1));
    }
    aliases.sort_unstable();
    aliases.dedup();
    aliases
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
#[path = "macos_tests.rs"]
mod tests;
