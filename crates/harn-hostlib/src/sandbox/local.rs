//! Local enforcement backend.
//!
//! Runs each command through `harn-vm`'s process sandbox, so the
//! kernel-level confinement (Landlock/seccomp on Linux, `sandbox-exec`
//! on macOS, Job Objects on Windows, `pledge`/`unveil` on OpenBSD) is
//! reused rather than reimplemented. Filesystem scope comes from the
//! session's mounts. On macOS, limited network policy is projected through
//! Harn's host-side egress proxy and a Seatbelt proxy-only rule; platforms
//! without an equivalent kernel boundary reject non-empty allowlists rather
//! than treating proxy environment variables as enforcement.

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
    harn_string, normalized_mount_target, ExecRequest, ExecResult, FilesystemAccess,
    FilesystemMount, NetworkPolicy, ResolvedMount, ResourceLimits, SandboxBackend,
    SandboxCapabilities, SandboxError, SandboxResult, SandboxSession, SandboxSessionId,
    SandboxSnapshot, SandboxSpec, SandboxState, MEMORY_MOUNT, OUTPUTS_MOUNT,
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
            network_policy: cfg!(target_os = "macos"),
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

        let network_proxy = local_network_proxy(&spec.network_policy)?;
        let session = Arc::new(LocalSession {
            id: id.clone(),
            tempdir,
            mounts: Mutex::new(mounts),
            network: Mutex::new(LocalNetworkState {
                policy: spec.network_policy,
                proxy: network_proxy,
            }),
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
        let session = self.session(session_id)?;
        let proxy = local_network_proxy(&policy)?;
        *session
            .network
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local network lock poisoned".to_string()))? =
            LocalNetworkState { policy, proxy };
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
    network: Mutex<LocalNetworkState>,
    limits: ResourceLimits,
    state: Mutex<SandboxState>,
    sandbox_profile: SandboxProfile,
}

#[derive(Debug)]
struct LocalNetworkState {
    policy: NetworkPolicy,
    proxy: Option<Arc<harn_vm::egress::ProcessEgressProxy>>,
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
        let source = self.harn_exec_source(&request)?;
        let (policy, proxy_guard) = self.execution_policy_with_proxy()?;

        let task = tokio::task::spawn_blocking(move || {
            let _proxy_guard = proxy_guard;
            run_harn_shell(source, policy)
        });
        task.await?
    }

    fn harn_exec_source(&self, request: &ExecRequest) -> SandboxResult<String> {
        let cwd = self.resolve_cwd(request.cwd.as_deref())?;
        let mut env = mount_env(&self.mounts()?);
        for key in request.env.keys() {
            validate_env_key(key)?;
        }
        env.extend(request.env.clone());

        let mut options = vec![
            format!("program: {}", harn_string(&request.command)),
            format!("args: {}", harn_string_list(&request.args)),
            format!("cwd: {}", harn_string(&cwd.display().to_string())),
            format!("env: {}", harn_string_dict(&env)),
        ];
        if let Some(stdin) = &request.stdin {
            options.push(format!("stdin: {}", harn_string(stdin)));
        }
        if let Some(timeout) = request.timeout.or(self.limits.wall_time) {
            options.push(format!("timeout_ms: {}", duration_millis(timeout)));
        }
        Ok(format!(
            "pipeline local_sandbox_exec(harness: Harness, task: unknown) {{ return harness.process.run({{{}}}) }}",
            options.join(", "),
        ))
    }

    #[cfg(test)]
    fn execution_policy(&self) -> SandboxResult<CapabilityPolicy> {
        self.execution_policy_with_proxy().map(|(policy, _)| policy)
    }

    fn execution_policy_with_proxy(
        &self,
    ) -> SandboxResult<(
        CapabilityPolicy,
        Option<Arc<harn_vm::egress::ProcessEgressProxy>>,
    )> {
        // The session root is always writable; declared mounts split by
        // their access so a `ReadOnly` mount lowers to a read-only root
        // the VM and OS sandbox both refuse to write.
        let mut roots = vec![self.tempdir.path().display().to_string()];
        let mut read_only_roots = Vec::new();
        for mount in self.mounts()? {
            if let Some(path) = mount.host_path {
                match mount.access {
                    FilesystemAccess::ReadWrite => roots.push(path.display().to_string()),
                    FilesystemAccess::ReadOnly => read_only_roots.push(path.display().to_string()),
                }
            }
        }
        let mut capabilities = BTreeMap::new();
        capabilities.insert("process".to_string(), vec!["run".to_string()]);
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

        let network = self
            .network
            .lock()
            .map_err(|_| SandboxError::Lifecycle("local network lock poisoned".to_string()))?;
        let proxy_guard = network.proxy.clone();
        let process_network_proxy = proxy_guard.as_ref().map(|proxy| proxy.endpoints());
        let network_enabled = !matches!(
            &network.policy,
            NetworkPolicy::Limited { allowed_hosts } if allowed_hosts.is_empty()
        );

        let policy = CapabilityPolicy {
            capabilities,
            workspace_roots: roots,
            read_only_roots,
            side_effect_level: Some(
                if network_enabled {
                    "network"
                } else {
                    "process_exec"
                }
                .to_string(),
            ),
            process_network_proxy,
            sandbox_profile: self.sandbox_profile,
            ..Default::default()
        };
        Ok((policy, proxy_guard))
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

fn local_network_proxy(
    policy: &NetworkPolicy,
) -> SandboxResult<Option<Arc<harn_vm::egress::ProcessEgressProxy>>> {
    match policy {
        NetworkPolicy::Unrestricted => Ok(None),
        NetworkPolicy::Limited { allowed_hosts } if allowed_hosts.is_empty() => Ok(None),
        NetworkPolicy::Limited { allowed_hosts } => {
            #[cfg(target_os = "macos")]
            {
                harn_vm::egress::ProcessEgressProxy::start_allowlist(allowed_hosts)
                    .map(Arc::new)
                    .map(Some)
                    .map_err(SandboxError::NetworkPolicy)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = allowed_hosts;
                Err(SandboxError::Unsupported {
                    backend: "local",
                    operation: "limited network allow-lists on this platform",
                })
            }
        }
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

fn harn_string_list(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| harn_string(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

fn harn_string_dict(values: &BTreeMap<String, String>) -> String {
    let fields = values
        .iter()
        .map(|(key, value)| format!("{}: {}", harn_string(key), harn_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{fields}}}")
}

fn duration_millis(duration: std::time::Duration) -> i64 {
    duration.as_millis().clamp(1, i64::MAX as u128) as i64
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
    // The typed Harness contract names the numeric field `exit_code`; legacy
    // process results used numeric `status`, while lifecycle-aware results now
    // use `status: "completed"`. Prefer the unambiguous typed field.
    let exit_code = dict_int_any(&map, &["exit_code", "status"])?;
    let timed_out = dict_bool_optional(&map, "timed_out")?.unwrap_or(false);
    Ok(ExecResult {
        stdout,
        stderr,
        exit_code,
        timed_out,
    })
}

fn dict_string(map: &harn_vm::value::DictMap, key: &str) -> SandboxResult<String> {
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

fn dict_int(map: &harn_vm::value::DictMap, key: &str) -> SandboxResult<i32> {
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

fn dict_int_any(map: &harn_vm::value::DictMap, keys: &[&str]) -> SandboxResult<i32> {
    for key in keys {
        if map.contains_key(*key) {
            return dict_int(map, key);
        }
    }
    Err(SandboxError::Exec(format!(
        "missing any of `{}` in exec result",
        keys.join("`, `")
    )))
}

fn dict_bool_optional(map: &harn_vm::value::DictMap, key: &str) -> SandboxResult<Option<bool>> {
    match map.get(key) {
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(SandboxError::Exec(format!(
            "expected `{key}` bool, got {}",
            other.display()
        ))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercises a real `sh -c` invocation with POSIX env expansion and
    // `printf`, so it only runs where a POSIX shell exists.
    // This proof executes the OS sandbox. The Linux CI runner intentionally
    // lacks Landlock, and OsHardened must fail closed there rather than weaken
    // the profile. Portable cwd admission is covered at the policy seam in
    // harn-vm; this real-process proof runs where the hardened backend exists.
    #[cfg(target_os = "macos")]
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

    #[cfg(unix)]
    #[tokio::test]
    async fn local_backend_timeout_is_enforced_without_shell_timeout_binary() {
        let backend = LocalSandbox::default();
        let session = backend.provision(SandboxSpec::default()).await.unwrap();

        let result = backend
            .exec(
                &session.id,
                ExecRequest {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 5".to_string()],
                    timeout: Some(std::time::Duration::from_millis(25)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(result.timed_out, "{result:?}");
        assert_eq!(result.exit_code, -1, "{result:?}");
    }

    #[tokio::test]
    async fn local_backend_applies_limited_network_policy_at_platform_boundary() {
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

        let result = backend
            .apply_network_policy(
                &session.id,
                NetworkPolicy::Limited {
                    allowed_hosts: vec!["example.com".to_string()],
                },
            )
            .await;

        #[cfg(target_os = "macos")]
        {
            result.expect("macOS Seatbelt can enforce proxy-only loopback access");
            let local = backend.session(&session.id).unwrap();
            let policy = local.execution_policy().unwrap();
            assert!(policy.process_network_proxy.is_some());
            assert_eq!(policy.side_effect_level.as_deref(), Some("network"));
        }
        #[cfg(not(target_os = "macos"))]
        assert!(matches!(
            result.unwrap_err(),
            SandboxError::Unsupported { .. }
        ));
    }

    #[tokio::test]
    async fn malformed_limited_network_policy_fails_before_session_provision() {
        let backend = LocalSandbox::default();
        let result = backend
            .provision(SandboxSpec {
                network_policy: NetworkPolicy::Limited {
                    allowed_hosts: vec!["[broken".to_string()],
                },
                ..Default::default()
            })
            .await;

        #[cfg(target_os = "macos")]
        assert!(matches!(
            result.unwrap_err(),
            SandboxError::NetworkPolicy(_)
        ));
        #[cfg(not(target_os = "macos"))]
        assert!(matches!(
            result.unwrap_err(),
            SandboxError::Unsupported { .. }
        ));
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

    #[tokio::test]
    async fn read_only_mounts_lower_to_read_only_roots() {
        let backend = LocalSandbox::default();
        let session = backend
            .provision(SandboxSpec {
                mounts: vec![FilesystemMount {
                    source: PathBuf::new(),
                    target: "/mnt/reference".to_string(),
                    access: FilesystemAccess::ReadOnly,
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        let local = backend.session(&session.id).unwrap();

        let policy = local.execution_policy().unwrap();

        // The canonical memory/outputs mounts plus the session root stay
        // writable; only the declared read-only mount lands in read_only_roots.
        assert!(
            policy
                .read_only_roots
                .iter()
                .any(|root| root.ends_with("reference")),
            "read-only mount should lower to read_only_roots, got {:?}",
            policy.read_only_roots
        );
        assert!(
            !policy
                .workspace_roots
                .iter()
                .any(|root| root.ends_with("reference")),
            "read-only mount must not appear among writable workspace_roots, got {:?}",
            policy.workspace_roots
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn local_backend_launches_in_read_only_mount_without_granting_write_or_escape() {
        let persona = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(persona.path().join("entrypoint.txt"), "PERSONA_OK").unwrap();

        let backend = LocalSandbox::default();
        let session = backend
            .provision(SandboxSpec {
                mounts: vec![FilesystemMount {
                    source: persona.path().to_path_buf(),
                    target: "/mnt/persona".to_string(),
                    access: FilesystemAccess::ReadOnly,
                }],
                ..Default::default()
            })
            .await
            .unwrap();

        let read = backend
            .exec(
                &session.id,
                ExecRequest {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), "cat entrypoint.txt".to_string()],
                    cwd: Some("/mnt/persona".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("a read-only mounted directory must be a launchable cwd");
        assert_eq!(read.exit_code, 0, "{read:?}");
        assert_eq!(read.stdout, "PERSONA_OK");

        let write = backend
            .exec(
                &session.id,
                ExecRequest {
                    command: "sh".to_string(),
                    args: vec![
                        "-c".to_string(),
                        "printf forbidden > should-not-exist.txt".to_string(),
                    ],
                    cwd: Some("/mnt/persona".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("the sandbox should execute and report the denied write");
        assert_ne!(
            write.exit_code, 0,
            "read-only mount accepted a write: {write:?}"
        );
        assert!(
            !persona.path().join("should-not-exist.txt").exists(),
            "read-only mount must remain immutable"
        );

        let escape = backend
            .exec(
                &session.id,
                ExecRequest {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), "pwd".to_string()],
                    cwd: Some(outside.path().display().to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("a cwd outside every declared mount must remain rejected");
        assert!(
            escape.to_string().contains("process cwd"),
            "escape rejection should identify the process axis: {escape}"
        );
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
