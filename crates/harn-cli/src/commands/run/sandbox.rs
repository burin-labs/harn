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

    // Disclose caller-declared grants that widened the default sandbox. This is
    // the narrow-scope counterpart to the `--no-sandbox` banner: a routine run
    // with no grants stays silent (no alarm fatigue), while a run that opened an
    // out-of-jail write/read root or subprocess network gets exactly one line
    // naming the delta. The filesystem, process, and egress defaults stay armed.
    if let Some(disclosure) = sandbox_grant_disclosure(options) {
        stderr.push_str(&disclosure);
    }

    RunSandboxScope {
        _execution_policy: execution_policy,
        _egress_policy: egress_policy,
        _ssrf_guard: ssrf_guard,
    }
}

/// Render the one-line disclosure naming exactly how caller-declared grants
/// widened the active sandbox profile, or `None` for an unmodified default run.
/// Kept separate from the `--no-sandbox` warning so the full escape hatch and a
/// narrow grant read differently in the terminal.
pub(super) fn sandbox_grant_disclosure(options: &RunSandboxOptions) -> Option<String> {
    if !options.enabled {
        return None;
    }
    let mut deltas: Vec<String> = Vec::new();
    if !options.write_roots.is_empty() {
        deltas.push(format!(
            "extra write root{}: {}",
            plural_suffix(options.write_roots.len()),
            display_grant_roots(&options.write_roots),
        ));
    }
    if !options.read_only_roots.is_empty() {
        deltas.push(format!(
            "extra read-only root{}: {}",
            plural_suffix(options.read_only_roots.len()),
            display_grant_roots(&options.read_only_roots),
        ));
    }
    if options.allow_process_network {
        deltas.push("subprocess network allowed".to_string());
    }
    if deltas.is_empty() {
        return None;
    }
    Some(format!("sandbox active; {}\n", deltas.join("; ")))
}

/// Render each grant to the exact path the sandbox jails it to, so the disclosed
/// string always equals the enforced write/read root.
fn display_grant_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|path| rendered_jail_root(path).display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The absolute, symlink-canonicalized path the sandbox actually jails a grant
/// to. `default_run_capability_policy` seeds the policy's workspace roots by
/// running each grant through `normalize_run_workspace_root`; the runtime then
/// renders every root to its jail path via `render_policy_root` (lexical
/// normalize + best-effort canonicalize). Disclosure and attestation reproduce
/// that full pipeline through the runtime's single owner so a reported path —
/// symlinks and `..` segments resolved — equals the enforced root and never
/// diverges from the jail. Canonicalization is best-effort and never panics.
fn rendered_jail_root(path: &Path) -> PathBuf {
    harn_vm::process_sandbox::render_policy_root(
        &normalize_run_workspace_root(path).display().to_string(),
    )
}

/// Render a policy's already-configured root strings to their enforced jail
/// paths through the same single owner the OS backends use.
fn render_policy_roots(roots: &[String]) -> Vec<String> {
    roots
        .iter()
        .map(|root| {
            harn_vm::process_sandbox::render_policy_root(root)
                .display()
                .to_string()
        })
        .collect()
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
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
        .map(|policy| render_policy_roots(&policy.workspace_roots))
        .unwrap_or_default();
    let read_only_roots = active_policy
        .as_ref()
        .map(|policy| render_policy_roots(&policy.read_only_roots))
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
        .map(|path| rendered_jail_root(path).display().to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_run_discloses_nothing() {
        assert_eq!(
            sandbox_grant_disclosure(&RunSandboxOptions::default()),
            None
        );
    }

    #[test]
    fn disabled_sandbox_discloses_nothing() {
        // `--no-sandbox` carries its own blanket warning; the grant disclosure
        // never fires for a disabled sandbox even if fields were populated.
        let mut options = RunSandboxOptions::disabled();
        options.write_roots = vec![PathBuf::from("/out/coordination")];
        assert_eq!(sandbox_grant_disclosure(&options), None);
    }

    #[test]
    fn single_write_root_names_the_delta() {
        let options =
            RunSandboxOptions::default().with_write_roots(vec![PathBuf::from("/out/coordination")]);
        assert_eq!(
            sandbox_grant_disclosure(&options).as_deref(),
            Some("sandbox active; extra write root: /out/coordination\n"),
        );
    }

    #[test]
    fn multiple_grants_join_on_one_line() {
        let options = RunSandboxOptions::sandboxed(true)
            .with_write_roots(vec![PathBuf::from("/out/a"), PathBuf::from("/out/b")])
            .with_read_only_roots(vec![PathBuf::from("/ref/shared")]);
        assert_eq!(
            sandbox_grant_disclosure(&options).as_deref(),
            Some(
                "sandbox active; extra write roots: /out/a, /out/b; \
                 extra read-only root: /ref/shared; subprocess network allowed\n"
            ),
        );
    }

    #[test]
    fn process_network_alone_is_disclosed() {
        let options = RunSandboxOptions::sandboxed(true);
        assert_eq!(
            sandbox_grant_disclosure(&options).as_deref(),
            Some("sandbox active; subprocess network allowed\n"),
        );
    }

    #[test]
    fn disclosure_names_the_canonical_jail_path_not_the_raw_grant() {
        // The whole point of the disclosure is precision, so a grant given
        // through a symlink or with `..` segments must be disclosed as the path
        // the sandbox actually jails to — the runtime canonicalizes at policy
        // render time — not the pre-canonical string the caller typed.
        let temp = tempfile::tempdir().expect("temp dir");
        let real = temp.path().join("real-state");
        std::fs::create_dir(&real).expect("create real dir");

        // Symlinked grant resolves to the real target it jails to.
        #[cfg(unix)]
        {
            let link = temp.path().join("link-state");
            std::os::unix::fs::symlink(&real, &link).expect("symlink");
            let jailed = rendered_jail_root(&link);
            assert_eq!(
                jailed,
                real.canonicalize().expect("canonical real"),
                "symlinked grant should jail to the real target"
            );
            let line = sandbox_grant_disclosure(
                &RunSandboxOptions::default().with_write_roots(vec![link.clone()]),
            )
            .expect("disclosure");
            assert!(
                line.contains(&jailed.display().to_string())
                    && !line.contains(&link.display().to_string()),
                "symlinked grant must disclose the canonical jail path: {line}"
            );
        }

        // A `..`-containing grant collapses to the same canonical jail path.
        let dotted = real.join("..").join("real-state");
        let jailed_dots = rendered_jail_root(&dotted);
        assert_eq!(
            jailed_dots,
            real.canonicalize().expect("canonical real"),
            "`..` grant should collapse to the real dir"
        );
        let dots_line =
            sandbox_grant_disclosure(&RunSandboxOptions::default().with_write_roots(vec![dotted]))
                .expect("disclosure");
        assert!(
            dots_line.contains(&jailed_dots.display().to_string()) && !dots_line.contains(".."),
            "`..` grant must disclose the collapsed jail path with no `..`: {dots_line}"
        );
    }
}
