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

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(test)]
use self::swiftpm::compatible_args as macos_sandbox_compatible_args;
use super::{
    normalized_process_roots, policy_allows_child_writes, policy_allows_network,
    process_sandbox_developer_toolchain_read_roots,
    process_sandbox_package_manager_config_read_roots, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_presets, process_sandbox_readonly_roots,
    process_sandbox_roots, process_spawn_error, spawn_error, unavailable, PrepareOutcome,
    ProcessCommandConfig, SandboxBackend,
};
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset, SandboxProfile};
use crate::value::VmError;

mod nested;
mod parent_environment;
pub(super) mod private_temp;
mod self_confinement;
pub(super) mod swiftpm;
mod swiftpm_toolset;
mod toolchain_roots;

const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

pub(super) struct Backend;

impl SandboxBackend for Backend {
    fn name() -> &'static str {
        "macos"
    }

    fn filesystem_mechanism() -> super::SandboxMechanism {
        super::SandboxMechanism::MacosSandboxExec
    }

    fn available() -> bool {
        Path::new(SANDBOX_EXEC_PATH).exists()
    }

    fn inherits_network_grants() -> bool {
        nested::process_is_sandboxed()
    }

    fn filesystem_available() -> bool {
        Self::available() && parent_environment::available()
    }

    fn filesystem_unavailable(profile: SandboxProfile) -> super::SandboxMechanismUnavailable {
        let mechanism = Self::filesystem_mechanism();
        match parent_environment::gap() {
            Some(gap) if Self::available() => {
                super::SandboxMechanismUnavailable::host_protection_unavailable(
                    mechanism, gap, profile,
                )
            }
            _ => super::SandboxMechanismUnavailable::new(
                mechanism,
                super::SandboxMechanismAvailability::AbsentOnHost,
                profile,
            ),
        }
    }

    fn confine_current_process(
        policy: &CapabilityPolicy,
    ) -> Result<super::SandboxMechanism, VmError> {
        self_confinement::confine(policy)?;
        Ok(Self::filesystem_mechanism())
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
        Self::run_to_output_in_session(program, args, config, policy, profile)
            .map(|(output, _)| output)
    }

    fn run_to_output_in_session(
        program: &str,
        args: &[String],
        config: &ProcessCommandConfig,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<(Output, u32), VmError> {
        let mut command = super::build_std_command::<Self>(program, args, policy, profile)?;
        super::apply_process_config(&mut command, config, Some(policy));
        let (output, session) = crate::op_interrupt::capture_output_interruptible_in_session(
            &mut command,
        )
        .map_err(|error| process_spawn_error(&error).unwrap_or_else(|| spawn_error(error)))?;
        match crate::process_sandbox::macos_wrapped_spawn_io_error(
            output.status.code().unwrap_or(-1),
            &output.stderr,
        ) {
            Some(error) => Err(spawn_error(error)),
            None => Ok((output, session)),
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
    parent_environment::protect(profile)?;
    match nested::nesting(policy) {
        nested::Nesting::NotNested => {}
        // Already confined at least as strictly: macOS would refuse a second
        // profile, so the child runs under this process's own. The program
        // still gets the arguments that adapt it to confinement.
        nested::Nesting::Inherit => {
            nested::report_inherited();
            return Ok(PrepareOutcome::WrappedExec {
                wrapper: program.to_string(),
                args: swiftpm::prepare_args(program, args, policy).map_err(|error| {
                    super::sandbox_rejection(format!(
                        "cannot prepare confined SwiftPM command: {error}"
                    ))
                })?,
            });
        }
        nested::Nesting::Unenforceable(narrowing) => {
            nested::report_unenforceable(&narrowing);
            return Err(super::sandbox_rejection(format!(
                "this process is already inside another sandbox, which allows {}; this run's \
                 policy denies that, and macOS cannot apply a second sandbox to enforce it, so \
                 the child was not started",
                narrowing.describe()
            )));
        }
    }
    let mut wrapped_args = vec![
        "-p".to_string(),
        render_profile_for_program(policy, program),
        "--".to_string(),
        program.to_string(),
    ];
    wrapped_args.extend(
        swiftpm::prepare_args(program, args, policy).map_err(|error| {
            super::sandbox_rejection(format!("cannot prepare confined SwiftPM command: {error}"))
        })?,
    );
    Ok(PrepareOutcome::WrappedExec {
        wrapper: SANDBOX_EXEC_PATH.to_string(),
        args: wrapped_args,
    })
}

/// Whether a child spawned under `policy` could write `target`, measured on
/// the confinement the spawn path will really apply (harn#9622).
///
/// An unsandboxed process wraps the child in the rendered profile, so that
/// profile is read. An already-sandboxed one runs the child under its own
/// inherited profile instead, so that profile is asked directly about the
/// target, and only a denial of every write operation there counts. A refused
/// spawn measures nothing.
pub(super) fn child_write_disposition(
    policy: &CapabilityPolicy,
    target: &Path,
) -> super::child_write::ChildWriteDisposition {
    use super::child_write::ChildWriteDisposition;
    match nested::nesting(policy) {
        nested::Nesting::NotNested => super::child_write::seatbelt_write_disposition(
            &render_profile_for_program(policy, "/bin/sh"),
            target,
        ),
        nested::Nesting::Inherit => {
            // `sandbox_check` cannot be trusted on a path that does not exist:
            // measured, it reports a denial the profile does not make. An
            // ancestor is no substitute, since a rule may name the target
            // alone, so a missing target measures nothing.
            if !target.exists() {
                return ChildWriteDisposition::Unknown;
            }
            let denied = ["file-write-data", "file-write-create", "file-write-unlink"]
                .iter()
                .all(|operation| nested::outer_denies(operation, Some(target)));
            if denied {
                ChildWriteDisposition::EnforcedReadOnly
            } else {
                ChildWriteDisposition::Unknown
            }
        }
        nested::Nesting::Unenforceable(_) => ChildWriteDisposition::Unknown,
    }
}

fn render_profile_for_program(policy: &CapabilityPolicy, program: &str) -> String {
    let mut developer_toolchain_read_roots = process_sandbox_developer_toolchain_read_roots(policy);
    developer_toolchain_read_roots.extend(toolchain_roots::go_read_root(policy, program));
    // Rendered with the other toolchain reads, so the read denylist emitted at
    // the end of the profile beats these grants too (last match wins).
    developer_toolchain_read_roots.extend(
        super::read_roots::path_grants::process_sandbox_path_entry_grants(policy)
            .into_iter()
            .map(|grant| grant.root),
    );
    developer_toolchain_read_roots.sort_unstable();
    developer_toolchain_read_roots.dedup();

    render_profile_with_extra_read_roots(
        policy,
        &developer_toolchain_read_roots,
        &process_sandbox_package_manager_config_read_roots(policy),
        &super::process_sandbox_developer_toolchain_cache_roots(policy),
    )
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
            sandbox_profile_escape(&root.display().to_string())
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
    if policy_allows_child_writes(policy) {
        for root in preset_write_roots(policy) {
            profile.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
        for root in &policy_write_roots {
            profile.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                sandbox_profile_escape(&root.display().to_string())
            ));
        }
        // `(allow file-write*)` with no filter is an unqualified grant, so the
        // aggregate rule is emitted only when it names at least one root. A
        // policy with no preset, policy, or cache write roots can still write
        // its workspace roots below, and nothing else.
        let aggregate_write_roots: Vec<_> = preset_write_roots(policy)
            .into_iter()
            .chain(policy_write_roots.iter().cloned())
            .chain(developer_toolchain_cache_roots.iter().cloned())
            .collect();
        if !aggregate_write_roots.is_empty() {
            profile.push_str("(allow file-write*");
            for root in &aggregate_write_roots {
                profile.push_str(&format!(
                    " (subpath \"{}\")",
                    sandbox_profile_escape(&root.display().to_string())
                ));
            }
            profile.push_str(")\n");
        }
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
    use super::policy::ProcessNetworkGrant;
    match ProcessNetworkGrant::for_policy(policy) {
        ProcessNetworkGrant::Public { proxy: Some(proxy) } => {
            for port in [proxy.http_port, proxy.socks_port] {
                profile.push_str(&format!(
                    "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
                ));
            }
        }
        ProcessNetworkGrant::Public { proxy: None } => profile.push_str("(allow network*)\n"),
        ProcessNetworkGrant::Loopback { .. } => {
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
        ProcessNetworkGrant::Denied => {}
    }
    // Unix-domain sockets are filtered by the socket file's path, so a
    // `subpath` filter on the three socket operations admits exactly the
    // sockets under the granted roots and nothing over IP: `subpath` never
    // matches an inet endpoint, and the loopback block above never matches a
    // socket file. Creating the socket (`socket(2)`) is not filtered; the
    // bind or connect is, which is where a build server dies today with a
    // bare EPERM. Both spellings of a `/tmp`- or `/var`-rooted path are
    // emitted for the same reason the write denies emit them.
    //
    // Sockets under the session temp dir come only with an explicit grant: the
    // dir is shared by every run in the workspace, so socket IPC there is the
    // host's decision, not a side effect of write access. A daemon
    // that binds under the host's shared temp dirs instead (sbt's boot server
    // under `/tmp/bsbt`) needs that root named in the grant.
    //
    // Binding creates the socket file, which is a file-write the bind rule
    // does not carry. A root outside every writable root therefore also gets
    // creation and removal, narrowed to socket vnodes so the root does not
    // become a place to write ordinary files.
    for root in unix_socket_profile_roots(policy) {
        for path in sandbox_profile_path_aliases(&root.display().to_string()) {
            let escaped = sandbox_profile_escape(&path);
            profile.push_str(&format!(
                "(allow network-bind (subpath \"{escaped}\"))\n\
                 (allow network-inbound (subpath \"{escaped}\"))\n\
                 (allow network-outbound (subpath \"{escaped}\"))\n\
                 (allow file-write-create file-write-unlink \
                 (require-all (subpath \"{escaped}\") (vnode-type SOCKET)))\n"
            ));
        }
    }
    // The read denylist is emitted LAST, after every allow in this function.
    //
    // `sandbox-exec` is last-match-wins, so position is the enforcement: a deny
    // written here beats the preset allows, the workspace and read-only allows,
    // the policy read roots, and the write block's read re-grants. That ordering
    // is the requirement, not an optimization: `PackageManagerConfig` grants
    // credential-bearing roots such as `~/.config/composer`, so a denial that
    // competed with presets instead of beating them would never fire on the
    // paths it exists for.
    //
    // `file-read*` covers metadata as well as data, so this also narrows the
    // global `(allow file-read-metadata)` at the top of the profile. Without
    // that, a denied path's existence and size would still leak to a child that
    // cannot open it.
    //
    // A read deny alone does not hold inside a writable root: the child could
    // rename the denied path, or any directory above it, and read it under the
    // new name (harn#9788). `file-write-unlink` covers rename, so denying it on
    // the denied subtree and on each directory above holds the denial while
    // every other entry in those directories stays writable.
    let mut pinned_ancestors = std::collections::BTreeSet::new();
    for root in read_deny_roots {
        for path in sandbox_profile_path_aliases(&root.display().to_string()) {
            let escaped = sandbox_profile_escape(&path);
            profile.push_str(&format!(
                "(deny file-read* (subpath \"{escaped}\"))\n\
                 (deny file-write-unlink (subpath \"{escaped}\"))\n"
            ));
            pinned_ancestors.extend(
                std::path::Path::new(&path)
                    .ancestors()
                    .skip(1)
                    .filter(|ancestor| ancestor.parent().is_some())
                    .map(|ancestor| ancestor.display().to_string()),
            );
        }
    }
    for ancestor in pinned_ancestors {
        profile.push_str(&format!(
            "(deny file-write-unlink (literal \"{}\"))\n",
            sandbox_profile_escape(&ancestor)
        ));
    }
    if let Some(socket) = &policy.process_sandbox.ssh_agent_socket {
        for path in sandbox_profile_path_aliases(socket) {
            let escaped = sandbox_profile_escape(&path);
            profile.push_str(&format!(
                "(allow network-outbound (literal \"{escaped}\"))\n\
                 (allow file-read-metadata (require-all (literal \"{escaped}\") (vnode-type SOCKET)))\n"
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

fn preset_read_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for preset in process_sandbox_presets(policy) {
        match preset {
            ProcessSandboxPreset::SystemRuntime => {
                roots.extend(
                    [
                        "/bin",
                        "/etc",
                        "/Library",
                        "/opt/homebrew",
                        "/private/etc",
                        "/private/var/select",
                        "/System",
                        "/usr",
                        "/var/select",
                    ]
                    .map(PathBuf::from),
                );
                // These resources already live under the preset's /etc and
                // /usr roots, but Seatbelt checks their resolved symlink
                // targets under /private/var. Admit only the actual public
                // resource, never the containing private directory.
                roots.extend(
                    ["/etc/resolv.conf", "/usr/share/zoneinfo"]
                        .into_iter()
                        .filter_map(|path| std::fs::canonicalize(path).ok()),
                );
            }
            ProcessSandboxPreset::DeveloperToolchains => roots.extend(
                [
                    "/Applications",
                    "/Library/Developer",
                    "/System/Library/Developer",
                ]
                .map(PathBuf::from),
            ),
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
    granted.extend(preset_write_roots(policy));
    granted.sort();
    granted.dedup();
    granted
}

fn preset_write_roots(policy: &CapabilityPolicy) -> Vec<std::path::PathBuf> {
    if process_sandbox_presets(policy).contains(&ProcessSandboxPreset::UserTemp) {
        user_temp_roots(policy)
    } else {
        Vec::new()
    }
}

/// The session's own temp dir, which the child's `TMPDIR`, `TMP`, and `TEMP`
/// name, including Foundation's native temporary-directory lookup. Not the host's
/// shared temp dirs: `/tmp` and `/var/folders` hold every other process's
/// files, so granting them lets a confined child read and overwrite its
/// neighbours'. Caches that default there are moved into the workspace by
/// the child's environment instead.
fn user_temp_roots(policy: &CapabilityPolicy) -> Vec<std::path::PathBuf> {
    // Provision before a server confines itself: subsequent spawns can read
    // the existing alias without modifying the ungranted shared parent.
    let _ = private_temp::prepare(policy);
    super::workspace_local_tmpdir(policy).into_iter().collect()
}

/// Socket-file roots the profile will admit: the explicit grant, plus the
/// UserTemp roots when that preset is on and the grant is non-empty. The temp
/// dir is shared by every run in the workspace, so socket IPC there stays a
/// host decision; a host running JVM tests (whose attach socket binds there)
/// grants a socket root.
fn unix_socket_profile_roots(policy: &CapabilityPolicy) -> Vec<std::path::PathBuf> {
    let mut roots = policy.process_sandbox.unix_socket_roots.clone();
    if !roots.is_empty()
        && process_sandbox_presets(policy).contains(&ProcessSandboxPreset::UserTemp)
    {
        roots.extend(
            user_temp_roots(policy)
                .iter()
                .map(|root| root.display().to_string()),
        );
    }
    normalized_process_roots(&roots)
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

#[cfg(test)]
#[path = "macos/write_roots_tests.rs"]
mod write_roots_tests;

#[cfg(test)]
#[path = "macos/external_root_tests.rs"]
mod external_root_tests;

#[cfg(test)]
#[path = "macos/socket_root_tests.rs"]
mod socket_root_tests;

#[cfg(test)]
#[path = "macos/child_write_nested_tests.rs"]
mod child_write_nested_tests;

#[cfg(test)]
#[path = "macos/writable_denial_tests.rs"]
mod writable_denial_tests;

#[cfg(test)]
#[path = "macos/parent_environment_tests.rs"]
mod parent_environment_tests;

#[cfg(test)]
#[path = "macos/system_runtime_read_tests.rs"]
mod system_runtime_read_tests;
