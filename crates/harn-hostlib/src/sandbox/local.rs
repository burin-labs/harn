//! Local enforcement backend.
//!
//! Runs each command through `harn-vm`'s process sandbox, so the
//! kernel-level confinement (Landlock/seccomp on Linux, `sandbox-exec`
//! on macOS, Job Objects on Windows, `pledge`/`unveil` on OpenBSD) is
//! reused rather than reimplemented. Filesystem scope comes from the
//! session's mounts; network egress is limited to deny-all or
//! allow-all, since per-host egress filtering for a local process is a
//! remote-backend capability (see [`SandboxCapabilities::network_policy`]).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};
use harn_vm::{compile_source, stdlib::register_vm_stdlib, Vm, VmValue};
use tempfile::TempDir;

use super::{
    duration_secs, harn_string, normalized_mount_target, sh_quote, ExecRequest, ExecResult,
    FilesystemAccess, FilesystemMount, NetworkPolicy, ResolvedMount, ResourceLimits,
    SandboxBackend, SandboxCapabilities, SandboxError, SandboxResult, SandboxSession,
    SandboxSessionId, SandboxSnapshot, SandboxSpec, SandboxState, MEMORY_MOUNT, OUTPUTS_MOUNT,
};

/// Configuration for a [`LocalSandbox`].
#[derive(Clone, Debug)]
pub struct LocalSandboxConfig {
    /// Directory under which session roots are created. When `None`,
    /// sessions are rooted under the current working directory.
    pub root_dir: Option<PathBuf>,
    /// The `harn-vm` sandbox profile applied to every command in this
    /// backend.
    pub sandbox_profile: SandboxProfile,
}

impl Default for LocalSandboxConfig {
    fn default() -> Self {
        Self {
            root_dir: None,
            sandbox_profile: SandboxProfile::OsHardened,
        }
    }
}

/// Local [`SandboxBackend`] that confines commands with `harn-vm`'s
/// process sandbox.
#[derive(Clone, Debug)]
pub struct LocalSandbox {
    config: LocalSandboxConfig,
    sessions: Arc<Mutex<HashMap<SandboxSessionId, Arc<LocalSession>>>>,
}

impl LocalSandbox {
    /// Construct a backend with the given configuration.
    pub fn new(config: LocalSandboxConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn session(&self, session_id: &SandboxSessionId) -> SandboxResult<Arc<LocalSession>> {
        self.sessions
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local session lock poisoned".to_string()))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| SandboxError::SessionNotFound(session_id.to_string()))
    }
}

impl Default for LocalSandbox {
    fn default() -> Self {
        Self::new(LocalSandboxConfig::default())
    }
}

#[async_trait]
impl SandboxBackend for LocalSandbox {
    fn name(&self) -> &'static str {
        "local"
    }

    fn capabilities(&self) -> SandboxCapabilities {
        SandboxCapabilities {
            local_process_sandbox: true,
            network_policy: false,
            snapshot: true,
            resume: true,
            suspend_on_idle: false,
        }
    }

    async fn provision(&self, mut spec: SandboxSpec) -> SandboxResult<SandboxSession> {
        let id = spec.session_id.take().unwrap_or_else(|| {
            SandboxSessionId(format!("local-{}", uuid::Uuid::now_v7().simple()))
        });
        let tempdir = match &self.config.root_dir {
            Some(root) => tempfile::Builder::new()
                .prefix("harn-sandbox-")
                .tempdir_in(root)?,
            None => tempfile::Builder::new()
                .prefix("harn-sandbox-")
                .tempdir_in(std::env::current_dir()?)?,
        };

        let root = tempdir.path().to_path_buf();
        let memory = root.join("mnt/memory");
        let outputs = root.join("mnt/session/outputs");
        std::fs::create_dir_all(&memory)?;
        std::fs::create_dir_all(&outputs)?;

        let mut mounts = vec![
            ResolvedMount {
                target: MEMORY_MOUNT.to_string(),
                access: FilesystemAccess::ReadWrite,
                host_path: Some(memory),
            },
            ResolvedMount {
                target: OUTPUTS_MOUNT.to_string(),
                access: FilesystemAccess::ReadWrite,
                host_path: Some(outputs),
            },
        ];
        for mount in spec.mounts {
            mounts.push(resolve_local_mount(&root, mount)?);
        }

        let session = Arc::new(LocalSession {
            id: id.clone(),
            tempdir,
            mounts: Mutex::new(mounts),
            network_policy: Mutex::new(spec.network_policy),
            limits: spec.limits,
            state: Mutex::new(SandboxState::Running),
            sandbox_profile: self.config.sandbox_profile,
        });

        self.sessions
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local session lock poisoned".to_string()))?
            .insert(id, session.clone());

        session.to_public()
    }

    async fn attach_filesystem(
        &self,
        session_id: &SandboxSessionId,
        mount: FilesystemMount,
    ) -> SandboxResult<SandboxSession> {
        let session = self.session(session_id)?;
        let resolved = resolve_local_mount(session.tempdir.path(), mount)?;
        session
            .mounts
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local mount lock poisoned".to_string()))?
            .push(resolved);
        session.to_public()
    }

    async fn apply_network_policy(
        &self,
        session_id: &SandboxSessionId,
        policy: NetworkPolicy,
    ) -> SandboxResult<SandboxSession> {
        if let NetworkPolicy::Limited { allowed_hosts } = &policy {
            if !allowed_hosts.is_empty() {
                return Err(SandboxError::Unsupported {
                    backend: "local",
                    operation: "limited network allow-lists",
                });
            }
        }
        let session = self.session(session_id)?;
        *session
            .network_policy
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local network lock poisoned".to_string()))? =
            policy;
        session.to_public()
    }

    async fn exec(
        &self,
        session_id: &SandboxSessionId,
        request: ExecRequest,
    ) -> SandboxResult<ExecResult> {
        let session = self.session(session_id)?;
        session.exec(request).await
    }

    async fn snapshot(&self, session_id: &SandboxSessionId) -> SandboxResult<SandboxSnapshot> {
        let session = self.session(session_id)?;
        Ok(SandboxSnapshot {
            session_id: session.id.clone(),
            backend: "local".to_string(),
            snapshot_id: format!("local:{}", session.id),
            metadata: BTreeMap::from([(
                "root".to_string(),
                session.tempdir.path().display().to_string(),
            )]),
        })
    }

    async fn resume(&self, session_id: &SandboxSessionId) -> SandboxResult<SandboxSession> {
        let session = self.session(session_id)?;
        *session
            .state
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local state lock poisoned".to_string()))? =
            SandboxState::Running;
        session.to_public()
    }

    async fn terminate(&self, session_id: &SandboxSessionId) -> SandboxResult<()> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local session lock poisoned".to_string()))?
            .remove(session_id)
            .ok_or_else(|| SandboxError::SessionNotFound(session_id.to_string()))?;
        *session
            .state
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local state lock poisoned".to_string()))? =
            SandboxState::Terminated;
        Ok(())
    }
}

#[derive(Debug)]
struct LocalSession {
    id: SandboxSessionId,
    tempdir: TempDir,
    mounts: Mutex<Vec<ResolvedMount>>,
    network_policy: Mutex<NetworkPolicy>,
    limits: ResourceLimits,
    state: Mutex<SandboxState>,
    sandbox_profile: SandboxProfile,
}

impl LocalSession {
    fn to_public(&self) -> SandboxResult<SandboxSession> {
        let mounts = self
            .mounts
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local mount lock poisoned".to_string()))?
            .clone();
        let state = self
            .state
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local state lock poisoned".to_string()))?
            .clone();
        Ok(SandboxSession {
            id: self.id.clone(),
            backend: "local".to_string(),
            state,
            mounts,
            metadata: BTreeMap::from([(
                "root".to_string(),
                self.tempdir.path().display().to_string(),
            )]),
        })
    }

    async fn exec(self: Arc<Self>, request: ExecRequest) -> SandboxResult<ExecResult> {
        if request.command.trim().is_empty() {
            return Err(SandboxError::InvalidRequest(
                "exec command cannot be empty".to_string(),
            ));
        }
        let timeout = request.timeout.or(self.limits.wall_time);
        let source = self.harn_exec_source(&request)?;
        let policy = self.execution_policy()?;

        let task = tokio::task::spawn_blocking(move || run_harn_shell(source, policy));
        match timeout {
            Some(timeout) => tokio::time::timeout(timeout, task)
                .await
                .map_err(|_| SandboxError::Exec("local exec timed out".to_string()))??,
            None => task.await?,
        }
    }

    fn harn_exec_source(&self, request: &ExecRequest) -> SandboxResult<String> {
        let cwd = self.resolve_cwd(request.cwd.as_deref())?;
        let mut shell = String::new();
        for (key, value) in mount_env(&self.mounts()?) {
            shell.push_str("export ");
            shell.push_str(&key);
            shell.push('=');
            shell.push_str(&sh_quote(&value));
            shell.push_str("; ");
        }
        for (key, value) in &request.env {
            validate_env_key(key)?;
            shell.push_str("export ");
            shell.push_str(key);
            shell.push('=');
            shell.push_str(&sh_quote(value));
            shell.push_str("; ");
        }
        if let Some(stdin) = &request.stdin {
            shell.push_str("printf %s ");
            shell.push_str(&sh_quote(stdin));
            shell.push_str(" | ");
        }
        if let Some(timeout) = request.timeout.or(self.limits.wall_time) {
            shell.push_str("timeout ");
            shell.push_str(&duration_secs(timeout).to_string());
            shell.push(' ');
        }
        shell.push_str(&sh_quote(&request.command));
        for arg in &request.args {
            shell.push(' ');
            shell.push_str(&sh_quote(arg));
        }
        Ok(format!(
            "pipeline local_sandbox_exec(task) {{ return shell_at({}, {}) }}",
            harn_string(&cwd.display().to_string()),
            harn_string(&shell),
        ))
    }

    fn execution_policy(&self) -> SandboxResult<CapabilityPolicy> {
        let mut roots = vec![self.tempdir.path().display().to_string()];
        for mount in self.mounts()? {
            if let Some(path) = mount.host_path {
                roots.push(path.display().to_string());
            }
        }
        let mut capabilities = BTreeMap::new();
        capabilities.insert("process".to_string(), vec!["exec".to_string()]);
        capabilities.insert(
            "workspace".to_string(),
            vec![
                "read_text".to_string(),
                "list".to_string(),
                "exists".to_string(),
                "write_text".to_string(),
                "delete".to_string(),
            ],
        );

        Ok(CapabilityPolicy {
            capabilities,
            workspace_roots: roots,
            side_effect_level: Some("process_exec".to_string()),
            sandbox_profile: self.sandbox_profile,
            ..Default::default()
        })
    }

    fn resolve_cwd(&self, cwd: Option<&str>) -> SandboxResult<PathBuf> {
        let Some(cwd) = cwd else {
            return Ok(self.tempdir.path().to_path_buf());
        };
        if cwd.trim().is_empty() {
            return Ok(self.tempdir.path().to_path_buf());
        }
        if let Some(path) = self.resolve_mount_path(cwd)? {
            return Ok(path);
        }
        let path = PathBuf::from(cwd);
        if path.is_absolute() {
            return Ok(path);
        }
        Ok(self.tempdir.path().join(path))
    }

    fn resolve_mount_path(&self, path: &str) -> SandboxResult<Option<PathBuf>> {
        if !path.trim_start().starts_with('/') {
            return Ok(None);
        }
        let normalized = normalized_mount_target(path)?;
        for mount in self.mounts()?.into_iter().rev() {
            if normalized == mount.target || normalized.starts_with(&(mount.target.clone() + "/")) {
                let Some(host_path) = mount.host_path else {
                    continue;
                };
                let suffix = normalized
                    .trim_start_matches(&mount.target)
                    .trim_start_matches('/');
                return Ok(Some(host_path.join(suffix)));
            }
        }
        Ok(None)
    }

    fn mounts(&self) -> SandboxResult<Vec<ResolvedMount>> {
        Ok(self
            .mounts
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local mount lock poisoned".to_string()))?
            .clone())
    }
}

fn resolve_local_mount(root: &Path, mount: FilesystemMount) -> SandboxResult<ResolvedMount> {
    let target = normalized_mount_target(&mount.target)?;
    let source = if mount.source.as_os_str().is_empty() {
        let relative = target.trim_start_matches('/');
        root.join(relative)
    } else if mount.source.is_absolute() {
        mount.source
    } else {
        root.join(mount.source)
    };
    std::fs::create_dir_all(&source)?;
    Ok(ResolvedMount {
        target,
        access: mount.access,
        host_path: Some(source),
    })
}

fn mount_env(mounts: &[ResolvedMount]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for mount in mounts {
        let Some(path) = &mount.host_path else {
            continue;
        };
        if mount.target == MEMORY_MOUNT {
            env.insert("HARN_MEMORY_DIR".to_string(), path.display().to_string());
        }
        if mount.target == OUTPUTS_MOUNT {
            env.insert("HARN_OUTPUTS_DIR".to_string(), path.display().to_string());
        }
    }
    env
}

fn validate_env_key(key: &str) -> SandboxResult<()> {
    if key.is_empty()
        || key
            .chars()
            .any(|ch| !(ch == '_' || ch.is_ascii_alphanumeric()))
        || key.as_bytes()[0].is_ascii_digit()
    {
        return Err(SandboxError::InvalidRequest(format!(
            "invalid environment key `{key}`"
        )));
    }
    Ok(())
}

fn run_harn_shell(source: String, policy: CapabilityPolicy) -> SandboxResult<ExecResult> {
    let chunk = compile_source(&source).map_err(SandboxError::Exec)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(SandboxError::Io)?;

    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let _guard = ExecutionPolicyGuard::push(policy);
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let value = vm.execute(&chunk).await.map_err(|error| {
                    SandboxError::Exec(format!("harn-vm process sandbox rejected exec: {error}"))
                })?;
                exec_result_from_value(value)
            })
            .await
    })
}

struct ExecutionPolicyGuard;

impl ExecutionPolicyGuard {
    fn push(policy: CapabilityPolicy) -> Self {
        push_execution_policy(policy);
        Self
    }
}

impl Drop for ExecutionPolicyGuard {
    fn drop(&mut self) {
        pop_execution_policy();
    }
}

fn exec_result_from_value(value: VmValue) -> SandboxResult<ExecResult> {
    let VmValue::Dict(map) = value else {
        return Err(SandboxError::Exec(format!(
            "expected exec result dict from harn-vm, got {}",
            value.display()
        )));
    };
    let stdout = dict_string(&map, "stdout")?;
    let stderr = dict_string(&map, "stderr")?;
    let exit_code = dict_int(&map, "status")?;
    Ok(ExecResult {
        stdout,
        stderr,
        exit_code,
        timed_out: false,
    })
}

fn dict_string(map: &BTreeMap<String, VmValue>, key: &str) -> SandboxResult<String> {
    match map.get(key) {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        Some(other) => Err(SandboxError::Exec(format!(
            "expected `{key}` string, got {}",
            other.display()
        ))),
        None => Err(SandboxError::Exec(format!(
            "missing `{key}` in exec result"
        ))),
    }
}

fn dict_int(map: &BTreeMap<String, VmValue>, key: &str) -> SandboxResult<i32> {
    match map.get(key) {
        Some(VmValue::Int(value)) => Ok(*value as i32),
        Some(other) => Err(SandboxError::Exec(format!(
            "expected `{key}` int, got {}",
            other.display()
        ))),
        None => Err(SandboxError::Exec(format!(
            "missing `{key}` in exec result"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_backend_execs_inside_session_outputs() {
        let backend = LocalSandbox::default();
        let session = backend.provision(SandboxSpec::default()).await.unwrap();

        let result = backend
            .exec(
                &session.id,
                ExecRequest {
                    command: "sh".to_string(),
                    args: vec![
                        "-c".to_string(),
                        "printf ok > \"$HARN_OUTPUTS_DIR/result.txt\" && cat \"$HARN_OUTPUTS_DIR/result.txt\""
                            .to_string(),
                    ],
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0, "{result:?}");
        assert_eq!(result.stdout, "ok");
    }

    #[tokio::test]
    async fn local_backend_rejects_limited_network_policy() {
        let backend = LocalSandbox::default();
        let session = backend.provision(SandboxSpec::default()).await.unwrap();
        let deny_all = backend
            .apply_network_policy(
                &session.id,
                NetworkPolicy::Limited {
                    allowed_hosts: Vec::new(),
                },
            )
            .await
            .expect("deny-all egress policy is enforceable locally");
        assert_eq!(deny_all.id, session.id);

        let err = backend
            .apply_network_policy(
                &session.id,
                NetworkPolicy::Limited {
                    allowed_hosts: vec!["example.com".to_string()],
                },
            )
            .await
            .unwrap_err();

        assert!(matches!(err, SandboxError::Unsupported { .. }));
    }

    #[tokio::test]
    async fn local_backend_defaults_to_os_hardened_sandbox_profile() {
        let backend = LocalSandbox::default();
        let session = backend.provision(SandboxSpec::default()).await.unwrap();
        let local = backend.session(&session.id).unwrap();

        let policy = local.execution_policy().unwrap();

        assert_eq!(policy.sandbox_profile, SandboxProfile::OsHardened);
    }

    #[tokio::test]
    async fn local_backend_threads_configured_sandbox_profile_into_policy() {
        let backend = LocalSandbox::new(LocalSandboxConfig {
            root_dir: None,
            sandbox_profile: SandboxProfile::Unrestricted,
        });
        let session = backend.provision(SandboxSpec::default()).await.unwrap();
        let local = backend.session(&session.id).unwrap();

        let policy = local.execution_policy().unwrap();

        assert_eq!(policy.sandbox_profile, SandboxProfile::Unrestricted);
    }

    #[test]
    fn mount_env_uses_canonical_mount_names() {
        let mounts = vec![ResolvedMount {
            target: OUTPUTS_MOUNT.to_string(),
            access: FilesystemAccess::ReadWrite,
            host_path: Some(PathBuf::from("/tmp/out")),
        }];
        assert_eq!(
            mount_env(&mounts).get("HARN_OUTPUTS_DIR"),
            Some(&"/tmp/out".to_string())
        );
    }
}
