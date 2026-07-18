use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSandboxOptions {
    /// Install the default `harn run` sandbox for this invocation.
    pub enabled: bool,
    /// Override the workspace root used by the default sandbox. This is
    /// intended for host-generated scripts whose source file lives outside
    /// the workspace they operate on.
    pub workspace_root: Option<PathBuf>,
    /// Extra writable filesystem roots mounted into the direct-run
    /// sandbox. These extend the write jail without disabling path
    /// enforcement or egress policy.
    pub write_roots: Vec<PathBuf>,
    /// Extra read-only filesystem roots. `path` resolving under one of
    /// these entries is scoped for reads, but writes still fail.
    pub read_only_roots: Vec<PathBuf>,
    /// Raise the direct-run side-effect ceiling to permit network-capable
    /// subprocesses without disabling filesystem or process confinement.
    pub allow_process_network: bool,
}

impl Default for RunSandboxOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            workspace_root: None,
            write_roots: Vec::new(),
            read_only_roots: Vec::new(),
            allow_process_network: false,
        }
    }
}

impl RunSandboxOptions {
    /// Construct the default sandbox with an explicit subprocess-network choice.
    pub fn sandboxed(allow_process_network: bool) -> Self {
        Self::default().with_process_network(allow_process_network)
    }

    /// Permit addressable sockets in commands spawned by this run while the
    /// rest of the direct-run sandbox remains active.
    pub fn with_process_network(mut self, enabled: bool) -> Self {
        self.allow_process_network = enabled;
        self
    }

    /// Disable the default direct-run sandbox and egress guard.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            workspace_root: None,
            write_roots: Vec::new(),
            read_only_roots: Vec::new(),
            allow_process_network: false,
        }
    }

    /// Constrain the default sandbox to an explicit workspace root.
    pub fn with_workspace_root(mut self, workspace_root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(workspace_root.into());
        self
    }

    /// Add writable roots to the default sandbox policy.
    pub fn with_write_roots<I>(mut self, write_roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.write_roots = write_roots.into_iter().collect();
        self
    }

    /// Add read-only roots to the default sandbox policy.
    pub fn with_read_only_roots<I>(mut self, read_only_roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.read_only_roots = read_only_roots.into_iter().collect();
        self
    }
}

struct ExecutionPolicyGuard;

impl Drop for ExecutionPolicyGuard {
    fn drop(&mut self) {
        harn_vm::orchestration::pop_execution_policy();
    }
}

pub(super) struct RunSandboxScope {
    _execution_policy: Option<ExecutionPolicyGuard>,
    _egress_policy: Option<harn_vm::egress::ExplicitEgressPolicyGuard>,
    _ssrf_guard: Option<harn_vm::egress::SsrfGuardScope>,
}

impl RunSandboxScope {
    fn disabled() -> Self {
        Self {
            _execution_policy: None,
            _egress_policy: None,
            _ssrf_guard: None,
        }
    }
}

pub(super) fn install_run_sandbox_scope(
    options: &RunSandboxOptions,
    workspace_root: &Path,
    stderr: &mut String,
) -> RunSandboxScope {
    if !options.enabled {
        stderr.push_str(
            "warning: harn run --no-sandbox disables filesystem, process, and egress sandbox defaults\n",
        );
        return RunSandboxScope::disabled();
    }

    let execution_policy = if harn_vm::orchestration::current_execution_policy().is_none() {
        harn_vm::orchestration::push_execution_policy(default_run_capability_policy(
            workspace_root,
            &options.write_roots,
            &options.read_only_roots,
            options.allow_process_network,
        ));
        Some(ExecutionPolicyGuard)
    } else {
        None
    };
    let egress_policy = Some(harn_vm::egress::require_explicit_egress_policy_for_host());
    // Default-on the SSRF private-address guard for outbound HTTP. Callers can
    // opt out with `egress_policy({block_private:"off"})` /
    // `HARN_EGRESS_BLOCK_PRIVATE=off`.
    let ssrf_guard = Some(harn_vm::egress::require_ssrf_guard_for_host());

    RunSandboxScope {
        _execution_policy: execution_policy,
        _egress_policy: egress_policy,
        _ssrf_guard: ssrf_guard,
    }
}

pub(super) fn default_run_capability_policy(
    workspace_root: &Path,
    write_roots: &[PathBuf],
    read_only_roots: &[PathBuf],
    allow_process_network: bool,
) -> harn_vm::orchestration::CapabilityPolicy {
    let mut workspace_roots = Vec::with_capacity(1 + write_roots.len());
    workspace_roots.push(
        normalize_run_workspace_root(workspace_root)
            .display()
            .to_string(),
    );
    workspace_roots.extend(
        write_roots
            .iter()
            .map(|path| normalize_run_workspace_root(path.as_path()))
            .map(|path| path.display().to_string()),
    );

    harn_vm::orchestration::CapabilityPolicy {
        workspace_roots,
        read_only_roots: read_only_roots
            .iter()
            .map(|path| normalize_run_workspace_root(path.as_path()))
            .map(|path| path.display().to_string())
            .collect(),
        side_effect_level: Some(
            if allow_process_network {
                harn_vm::tool_annotations::SideEffectLevel::Network
            } else {
                harn_vm::tool_annotations::SideEffectLevel::ProcessExec
            }
            .as_str()
            .to_string(),
        ),
        sandbox_profile: harn_vm::orchestration::SandboxProfile::Worktree,
        ..harn_vm::orchestration::CapabilityPolicy::default()
    }
}

fn normalize_run_workspace_root(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn default_run_workspace_root(
    project_root: Option<&Path>,
    source_parent: &Path,
) -> PathBuf {
    project_root
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| source_parent.to_path_buf())
}

pub(super) fn run_sandbox_attestation(sandbox: &RunSandboxOptions) -> serde_json::Value {
    let active_policy = harn_vm::orchestration::current_execution_policy();
    let active = active_policy.is_some();
    let workspace_roots = active_policy
        .as_ref()
        .map(|policy| policy.workspace_roots.clone())
        .unwrap_or_default();
    let read_only_roots = active_policy
        .as_ref()
        .map(|policy| policy.read_only_roots.clone())
        .unwrap_or_default();
    let profile = active_policy
        .as_ref()
        .map(|policy| policy.sandbox_profile.as_str())
        .unwrap_or("unrestricted");
    let side_effect_level = active_policy
        .as_ref()
        .and_then(|policy| policy.side_effect_level.as_deref())
        .unwrap_or(harn_vm::tool_annotations::SideEffectLevel::MAX.as_str());
    let process_network_enabled =
        harn_vm::tool_annotations::SideEffectLevel::rank_str(side_effect_level)
            >= harn_vm::tool_annotations::SideEffectLevel::Network.rank();
    let egress = if sandbox.enabled {
        "explicit_policy_required"
    } else if active {
        "host_policy"
    } else {
        "unrestricted"
    };
    let write_roots = sandbox
        .write_roots
        .iter()
        .map(|path| normalize_run_workspace_root(path).display().to_string())
        .collect::<Vec<_>>();

    serde_json::json!({
        "run_default_enabled": sandbox.enabled,
        "active": active,
        "workspace_roots": workspace_roots,
        "write_roots": write_roots,
        "read_only_roots": read_only_roots,
        "profile": profile,
        "process_network_requested": sandbox.allow_process_network,
        "process_network_enabled": process_network_enabled,
        "side_effect_level": side_effect_level,
        "egress": egress,
    })
}
