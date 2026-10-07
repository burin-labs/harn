//! Filesystem scope decisions and scoped child execution over declared grants.

use super::*;
use crate::orchestration::ProcessSandboxPreset;

/// Check whether `path` is inside the active policy's workspace roots.
///
/// Returns `Ok(())` when no execution policy is active, when the active
/// profile does not enforce path scope, when the normalized path
/// falls within a writable workspace root, or — for [`FsAccess::Read`]
/// only — when it falls within a read-only root. A write/delete that
/// resolves under a read-only root is rejected with `read_only` set, as
/// is any access that falls outside every configured root.
///
/// This is the public, `VmError`-free entry point embedders use to apply
/// workspace-root scoping to their own host calls. The in-crate
/// `harness.fs.*` builtins funnel through [`enforce_fs_path`], which wraps
/// this with a `VmError`; both share the same path normalization and
/// rejection text.
pub fn check_fs_path_scope(path: &Path, access: FsAccess) -> Result<(), SandboxViolation> {
    check_fs_path_scope_with_roots(path, access, PATH_SCOPE_ROOTS.with(std::cell::Cell::get))
}

/// Authorize Git-selected metadata using existing grants, before following
/// repository pointers. A pointer must not grant access to its own target.
pub fn check_git_metadata_path_scope(path: &Path) -> Result<(), SandboxViolation> {
    check_fs_path_scope_with_roots(path, FsAccess::Read, PathScopeRoots::Declared)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathScopeRoots {
    Assembled,
    Declared,
}

thread_local! {
    static PATH_SCOPE_ROOTS: std::cell::Cell<PathScopeRoots> = const { std::cell::Cell::new(PathScopeRoots::Assembled) };
}

pub(super) fn with_declared_path_roots<T>(operation: impl FnOnce() -> T) -> T {
    struct RestoreRoots(PathScopeRoots);
    impl Drop for RestoreRoots {
        fn drop(&mut self) {
            PATH_SCOPE_ROOTS.with(|selection| selection.set(self.0));
        }
    }
    let previous = PATH_SCOPE_ROOTS.with(|selection| selection.replace(PathScopeRoots::Declared));
    let _restore = RestoreRoots(previous);
    operation()
}

pub(super) fn declared_path_roots_active() -> bool {
    PATH_SCOPE_ROOTS.with(|selection| selection.get() == PathScopeRoots::Declared)
}

fn check_fs_path_scope_with_roots(
    path: &Path,
    access: FsAccess,
    root_selection: PathScopeRoots,
) -> Result<(), SandboxViolation> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if !policy.sandbox_profile.enforces_path_scope() {
        return Ok(());
    }
    // Standard process I/O device files are not workspace filesystem
    // mutations: writing to /dev/stdout, /dev/stderr, or /dev/null (and the
    // numeric /dev/fd/<N> descriptors they alias) targets the process's own
    // output streams, not the sandboxed tree. A pipeline that falls back to
    // /dev/stdout for debug output must not read as a sandbox violation, so
    // allow these regardless of the configured roots. Matched on the
    // lexically-normalized path (not the canonicalized form): canonicalize()
    // rewrites /dev/stdout to a per-process /dev/fd/<…>.output alias that no
    // longer looks like a standard device. Kept deliberately narrow — only
    // the well-known device files, no broader /dev access.
    if access_is_exempt_from_scope(path, access) {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let scope = match root_selection {
        PathScopeRoots::Assembled => scope_memo::scope_roots(&policy, access),
        PathScopeRoots::Declared => std::rc::Rc::new(scope_memo::ScopeRoots {
            workspace: base_workspace_roots(&policy),
            read_only: read_roots::normalized_declared_read_only_roots(&policy),
            read_deny: process_sandbox_read_deny_roots(&policy),
        }),
    };
    let roots = scope.workspace.clone();
    // The denylist is checked BEFORE any grant, because it must beat all of
    // them. A workspace root, a read-only root, and a preset are each a reason
    // to allow; this is the one reason to refuse, and a subtraction that ran
    // after the grants would never fire on the paths that matter (a credential
    // under preset-granted `~/.config/composer` is exactly that case).
    if access == FsAccess::Read && path_is_denied(&candidate, &scope.read_deny) {
        return Err(SandboxViolation {
            attempted: candidate,
            roots,
            access,
            read_only: false,
        });
    }
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    let within_read_only = scope
        .read_only
        .iter()
        .any(|root| path_is_within(&candidate, root));
    if within_read_only && access == FsAccess::Read {
        return Ok(());
    }
    Err(SandboxViolation {
        attempted: candidate,
        roots,
        access,
        read_only: within_read_only,
    })
}

/// Run through the existing confined child owner without deriving grants from
/// mutable repository pointers. The selection spans construction and execution.
/// Restricted callers refuse if read confinement cannot actually be enforced.
pub fn command_output_with_declared_roots(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
) -> Result<Output, VmError> {
    with_declared_path_roots(|| {
        let _policy_scope = if let Some(mut policy) =
            crate::orchestration::current_execution_policy()
                .filter(|policy| policy.sandbox_profile.enforces_path_scope())
        {
            // Identity reads need executable/runtime access, not ambient user
            // temp or package-manager configuration. Narrow existing presets;
            // explicit roots and subtractive deny rules remain authoritative.
            policy.process_sandbox.presets = Some(
                policy
                    .process_sandbox
                    .effective_presets()
                    .into_iter()
                    .filter(|preset| {
                        matches!(
                            preset,
                            ProcessSandboxPreset::SystemRuntime
                                | ProcessSandboxPreset::DeveloperToolchains
                        )
                    })
                    .collect(),
            );
            crate::orchestration::push_execution_policy(policy);
            Some(ProcessSandboxScopeGuard { pushed: true })
        } else {
            None
        };
        let restricted_config;
        let config = if _policy_scope.is_some() {
            restricted_config = without_user_git_config(config);
            &restricted_config
        } else {
            config
        };
        if _policy_scope.is_some() {
            let row = enforcement::active_enforcement();
            if active_sandbox_policy().is_none()
                || !active_backend_filesystem_available()
                || !row.is_some_and(|row| {
                    [
                        enforcement::ConfinementDimension::Reads,
                        enforcement::ConfinementDimension::CredentialReads,
                    ]
                    .into_iter()
                    .all(|dimension| row.cell(dimension) == enforcement::Enforcement::Enforced)
                })
            {
                return Err(sandbox_rejection(
                    "declared-root command requires enforced filesystem read confinement on this platform".into(),
                ));
            }
        }
        command_output(program, args, config)
    })
}

/// Git's global and system config are read roots only under the
/// package-manager preset this runner withholds: that preset is what grants
/// `~/.gitconfig` and the includes it names. A child pointed at those files
/// would hit the jail and Git treats an unreadable config as fatal, so the
/// confined child reads repository config alone. Repository includes still
/// resolve, and still stop at the declared roots.
fn without_user_git_config(config: &ProcessCommandConfig) -> ProcessCommandConfig {
    let mut config = config.clone();
    for (key, value) in [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ] {
        config
            .env
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
        config
            .env_remove
            .retain(|removed| !removed.eq_ignore_ascii_case(key));
        config.env.push((key.to_string(), value.to_string()));
    }
    config
}
