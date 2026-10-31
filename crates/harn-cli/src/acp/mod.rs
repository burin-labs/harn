use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harn_serve::{
    AcpProfileConfig, AcpRuntimeConfigurator, AcpSandboxConfig, AcpServerConfig, AuthPolicy,
};

/// Opts a server out of warming the code index when a session opens.
pub(crate) const DISABLE_CODE_INDEX_WARM_ENV: &str = "HARN_DISABLE_CODE_INDEX_WARM";

fn code_index_warm_disabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Hostlib handles per session project root, so a session warm fills the
/// code index, and holds the single-flight gate, that every later VM over
/// that workspace installs.
///
/// A root's handles stay while a session over it is open or its index has
/// work in flight, so a second workspace opening never strands the first
/// one's warm. They are released at the last close, or when index work
/// still running at that close ends.
#[cfg(feature = "hostlib")]
#[derive(Default)]
struct WorkspaceHostlib {
    by_root: Arc<std::sync::Mutex<WorkspaceMap>>,
}

#[cfg(feature = "hostlib")]
type WorkspaceMap = std::collections::HashMap<std::path::PathBuf, WorkspaceEntry>;

#[cfg(feature = "hostlib")]
struct WorkspaceEntry {
    handles: harn_hostlib::DefaultHostlibHandles,
    open_sessions: usize,
}

#[cfg(feature = "hostlib")]
impl WorkspaceHostlib {
    fn lock(&self) -> std::sync::MutexGuard<'_, WorkspaceMap> {
        Self::lock_map(&self.by_root)
    }

    fn lock_map(
        by_root: &std::sync::Mutex<WorkspaceMap>,
    ) -> std::sync::MutexGuard<'_, WorkspaceMap> {
        by_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn entry<'a>(by_root: &'a mut WorkspaceMap, project_root: &Path) -> &'a mut WorkspaceEntry {
        by_root
            .entry(project_root.to_path_buf())
            .or_insert_with(|| WorkspaceEntry {
                handles: crate::default_hostlib_handles(),
                open_sessions: 0,
            })
    }

    /// Drop every root no open session needs and no index work holds.
    fn release_unused(by_root: &mut WorkspaceMap) {
        by_root.retain(|_, entry| entry.open_sessions > 0 || !entry.handles.code_index.is_idle());
    }

    /// The handles a VM over `project_root` installs.
    fn handles_for(&self, project_root: &Path) -> harn_hostlib::DefaultHostlibHandles {
        Self::entry(&mut self.lock(), project_root).handles.clone()
    }

    /// Count a session opening over `project_root` and return its handles.
    fn open_session(&self, project_root: &Path) -> harn_hostlib::DefaultHostlibHandles {
        let mut by_root = self.lock();
        Self::release_unused(&mut by_root);
        let entry = Self::entry(&mut by_root, project_root);
        entry.open_sessions += 1;
        entry.handles.clone()
    }

    /// Returns the thread that releases a root whose index work outlived its
    /// last session, if one was scheduled.
    fn close_session(&self, project_root: &Path) -> Option<std::thread::JoinHandle<()>> {
        let mut by_root = self.lock();
        if let Some(entry) = by_root.get_mut(project_root) {
            entry.open_sessions = entry.open_sessions.saturating_sub(1);
        }
        Self::release_unused(&mut by_root);
        // No later session event is guaranteed, so a root no session needs
        // but whose index work is still running is released when it ends.
        let entry = by_root
            .get(project_root)
            .filter(|entry| entry.open_sessions == 0)?;
        let code_index = entry.handles.code_index.clone();
        let map = Arc::clone(&self.by_root);
        harn_parser::runtime_stack::builder()
            .name("harn-acp-index-release".to_string())
            .spawn(move || {
                code_index.wait_until_idle();
                Self::release_unused(&mut Self::lock_map(&map));
            })
            .inspect_err(|error| {
                tracing::warn!(
                    target: "harn_cli::acp",
                    %error,
                    "could not schedule the code-index release; it waits for the next session event",
                );
            })
            .ok()
    }
}

struct CliAcpRuntimeConfigurator {
    #[cfg(feature = "hostlib")]
    hostlib: WorkspaceHostlib,
    warm_code_index: bool,
}

impl CliAcpRuntimeConfigurator {
    fn from_env() -> Self {
        let disabled = std::env::var(DISABLE_CODE_INDEX_WARM_ENV).ok();
        Self::new(!code_index_warm_disabled(disabled.as_deref()))
    }

    fn new(warm_code_index: bool) -> Self {
        Self {
            #[cfg(feature = "hostlib")]
            hostlib: WorkspaceHostlib::default(),
            warm_code_index,
        }
    }
}

#[async_trait(?Send)]
impl AcpRuntimeConfigurator for CliAcpRuntimeConfigurator {
    async fn configure(
        &self,
        vm: &mut harn_vm::Vm,
        source_path: Option<&Path>,
    ) -> Result<(), String> {
        // Hostlib registration is independent of the package/extension flow:
        // even a `harn run` invocation that hasn't loaded a manifest should
        // see the `hostlib_*` builtins so callers can probe the surface.
        // Behind the `hostlib` cargo feature (default-on); see
        // `crates/harn-hostlib/README.md` for the boundary contract.
        #[cfg(feature = "hostlib")]
        {
            let handles = match vm.explicit_project_root() {
                Some(root) => self.hostlib.handles_for(root),
                None => crate::default_hostlib_handles(),
            };
            crate::install_hostlib_with_handles(vm, &handles);
        }

        // Install the lazy neural injection-classifier loader (Layer 2, guard
        // backend). Built only under `guard-neural`; the runtime fires it the
        // first time a `local-ml` policy scores untrusted content. Capturing the
        // project base dir lets `harn-guard` resolve the installed model store.
        #[cfg(feature = "guard-neural")]
        {
            let base_dir = source_path
                .and_then(std::path::Path::parent)
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf();
            harn_vm::security::set_injection_classifier_loader(Box::new(move |selector| {
                harn_guard::load_classifier(&base_dir, selector)
            }));
        }

        let Some(path) = source_path else {
            return Ok(());
        };

        // ACP is another execution transport for the same file-backed project,
        // so its VM must receive the same manifest-declared authority as `run`
        // and source execution before trigger or hook modules are loaded.
        crate::compiler_context::enable_trusted_host_dispatch_for_source(vm, path)
            .map_err(|error| format!("failed to enable trusted host dispatch: {error}"))?;

        let extensions = crate::package::load_runtime_extensions(path);
        crate::package::install_runtime_extensions(&extensions);
        crate::package::install_manifest_triggers(vm, &extensions)
            .await
            .map_err(|error| format!("failed to install manifest triggers: {error}"))?;
        crate::package::install_manifest_hooks(vm, &extensions)
            .await
            .map_err(|error| format!("failed to install manifest hooks: {error}"))?;
        Ok(())
    }

    fn warm_session_workspace(&self, project_root: &Path) {
        #[cfg(feature = "hostlib")]
        {
            let handles = self.hostlib.open_session(project_root);
            if !self.warm_code_index {
                return;
            }
            let outcome = handles.code_index.warm_session(project_root);
            tracing::debug!(
                target: "harn_cli::acp",
                root = %project_root.display(),
                ?outcome,
                "code-index session warm requested",
            );
        }
        #[cfg(not(feature = "hostlib"))]
        let _ = project_root;
    }

    fn release_session_workspace(&self, project_root: &Path) {
        // The release thread runs detached; only tests join it.
        #[cfg(feature = "hostlib")]
        drop(self.hostlib.close_session(project_root));
        #[cfg(not(feature = "hostlib"))]
        let _ = project_root;
    }
}

pub(crate) fn server_config(
    pipeline: Option<String>,
    auth_policy: AuthPolicy,
) -> Result<AcpServerConfig, String> {
    let host_inference_boundary = harn_vm::llm::api::InferenceBoundary::capture_process()?;
    Ok(server_config_with_boundary(
        pipeline,
        auth_policy,
        host_inference_boundary,
    ))
}

pub(crate) fn server_config_with_boundary(
    pipeline: Option<String>,
    auth_policy: AuthPolicy,
    host_inference_boundary: Option<harn_vm::llm::api::InferenceBoundary>,
) -> AcpServerConfig {
    let extensions = pipeline
        .as_deref()
        .map(Path::new)
        .map(crate::package::load_runtime_extensions)
        .unwrap_or_default();
    let mut config = AcpServerConfig::new(pipeline)
        .with_auth_policy(auth_policy)
        .with_runtime_configurator(Arc::new(CliAcpRuntimeConfigurator::from_env()))
        .with_llm_overrides(extensions.llm, extensions.capabilities);
    config.host_inference_boundary = host_inference_boundary;
    config
}

pub(crate) fn ensure_acp_event_log(pipeline: Option<&str>) {
    if harn_vm::event_log::active_event_log().is_none() {
        let base_dir = pipeline
            .map(Path::new)
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("."));
        if let Err(error) = harn_vm::event_log::install_default_for_base_dir(base_dir) {
            cli_diagnostic!(
                "warning: ACP session replay disabled: failed to initialize EventLog for {}: {error}",
                base_dir.display()
            );
        }
    }
}

pub(crate) async fn run_acp_server(
    pipeline: Option<&str>,
    auth_policy: AuthPolicy,
    trace: bool,
    profile: AcpProfileConfig,
    sandbox: AcpSandboxConfig,
    session_environment: Option<harn_vm::security::EnvironmentPolicyKind>,
    confinement: Option<harn_serve::AcpServerConfinement>,
) -> Result<(), String> {
    ensure_acp_event_log(pipeline);
    if trace {
        harn_vm::llm::enable_tracing();
    }
    let mut config = server_config(pipeline.map(str::to_string), auth_policy)?
        .with_profile(profile)
        .with_sandbox(sandbox);
    if let Some(kind) = session_environment {
        config = config.with_launcher_session_environment(kind);
    }
    // Confine last, once everything that reads the host has run. On Linux
    // the process was confined before its runtime started instead.
    let already_confined = harn_vm::process_sandbox::current_process_confinement().is_some();
    if let Some(confinement) = confinement.filter(|_| !already_confined) {
        harn_serve::confine_acp_server_process(&config, &confinement)
            .map_err(|error| format!("--confine-workspace: {error}"))?;
    }
    harn_serve::run_acp_server(config).await;
    if trace {
        eprint!("{}", crate::commands::run::render_trace_summary());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn configure_and_resolve_fixture(declared: bool) -> Result<(), String> {
        harn_vm::reset_thread_local_state();
        crate::compiler_context::ensure_builtin_signatures_installed();
        let project = tempfile::tempdir().expect("temp project");
        let script =
            crate::tests::common::host_dispatch_project::write_host_dispatch_trigger_project(
                project.path(),
                declared,
                r#"
pub fn on_tick(_event) -> nil {
  const _ = host_call("runtime.pipeline_input", {})
  return nil
}
"#,
            );
        let mut vm = harn_vm::new_vm();
        harn_runtime::register_vm_stdlib(&mut vm);
        let result = async {
            CliAcpRuntimeConfigurator::new(false)
                .configure(&mut vm, Some(&script))
                .await?;
            let extensions = crate::package::load_runtime_extensions(&script);
            let collected = crate::package::collect_manifest_triggers(&mut vm, &extensions)
                .await
                .map_err(|error| error.to_string())?;
            let crate::package::CollectedTriggerHandler::Local { callable, .. } =
                &collected[0].handler
            else {
                return Err("fixture trigger must use a local handler".to_string());
            };
            vm.resolve_callable(callable)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        .await;
        harn_vm::reset_thread_local_state();
        result
    }

    #[tokio::test]
    async fn acp_honors_manifest_trusted_host_dispatch_before_installing_triggers() {
        configure_and_resolve_fixture(true)
            .await
            .expect("declared ACP project resolves its privileged trigger on dispatch");

        let error = configure_and_resolve_fixture(false)
            .await
            .expect_err("undeclared ACP project remains unprivileged on dispatch");
        assert!(
            error.contains("host_call") && error.contains("not callable source API"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn the_code_index_warm_opt_out_reads_boolean_values() {
        for value in ["1", "true", "YES", " on "] {
            assert!(code_index_warm_disabled(Some(value)), "{value}");
        }
        for value in [None, Some(""), Some("0"), Some("false"), Some("off")] {
            assert!(!code_index_warm_disabled(value), "{value:?}");
        }
    }

    /// The warm fills the index the server's VMs share, and an opted-out
    /// server leaves it empty.
    #[test]
    fn a_session_warm_fills_the_shared_code_index_unless_opted_out() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(
            workspace.path().join("src/lib.rs"),
            "pub fn warmed() -> i32 { 1 }\n",
        )
        .unwrap();
        let root = workspace.path().canonicalize().unwrap();
        for warm in [true, false] {
            let configurator = CliAcpRuntimeConfigurator::new(warm);
            configurator.warm_session_workspace(&root);
            let code_index = configurator.hostlib.handles_for(&root).code_index;
            code_index.wait_until_idle();
            let shared = code_index.shared();
            let live_root = shared
                .lock()
                .unwrap()
                .as_ref()
                .map(|state| state.root.clone());
            let expected = warm.then(|| root.clone());
            assert_eq!(live_root, expected, "warm={warm}");
        }
    }

    /// A workspace's index lives while a session over it is open, and is
    /// released once its last session closes and no work is in flight.
    #[test]
    fn closing_the_last_session_over_a_workspace_releases_its_index() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
        let root = workspace.path().canonicalize().unwrap();
        let configurator = CliAcpRuntimeConfigurator::new(true);
        configurator.warm_session_workspace(&root);
        configurator.warm_session_workspace(&root);
        let warmed = configurator.hostlib.handles_for(&root).code_index.shared();
        configurator
            .hostlib
            .handles_for(&root)
            .code_index
            .wait_until_idle();

        configurator.release_session_workspace(&root);
        let while_open = configurator.hostlib.handles_for(&root).code_index.shared();
        assert!(
            Arc::ptr_eq(&warmed, &while_open),
            "a session is still open over the workspace"
        );

        configurator.release_session_workspace(&root);
        let after_close = configurator.hostlib.handles_for(&root).code_index.shared();
        assert!(
            !Arc::ptr_eq(&warmed, &after_close),
            "the last close must release the warmed index"
        );
        assert!(after_close.lock().unwrap().is_none());
    }

    /// A warm still running when the last session closes is released when it
    /// ends, even though no later session event arrives.
    #[test]
    fn a_warm_running_at_the_last_close_is_released_when_it_ends() {
        let workspace = tempfile::tempdir().unwrap();
        for file in 0..400 {
            std::fs::write(
                workspace.path().join(format!("m{file}.rs")),
                "pub fn f() -> u32 { 1 }\n",
            )
            .unwrap();
        }
        let root = workspace.path().canonicalize().unwrap();
        let configurator = CliAcpRuntimeConfigurator::new(true);
        configurator.warm_session_workspace(&root);
        // Holding the index lock keeps the warm from publishing, so the
        // close below deterministically meets in-flight work.
        let shared = configurator.hostlib.handles_for(&root).code_index.shared();
        let publish_gate = shared.lock().unwrap();
        assert!(publish_gate.is_none(), "the warm finished before the gate");
        let release = configurator.hostlib.close_session(&root);
        let retained = || configurator.hostlib.lock().contains_key(&root);
        assert!(retained(), "in-flight work keeps the root");

        drop(publish_gate);
        release
            .expect("a release is scheduled for in-flight work")
            .join()
            .unwrap();
        assert!(!retained(), "the root must be released once its warm ends");
    }

    /// A second workspace opening must not strand the first one's warm: the
    /// first workspace's VMs still install the index its warm filled.
    #[test]
    fn opening_another_workspace_keeps_the_first_workspaces_warm_index() {
        let workspaces = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let roots: Vec<_> = workspaces
            .iter()
            .map(|dir| {
                std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
                dir.path().canonicalize().unwrap()
            })
            .collect();
        let configurator = CliAcpRuntimeConfigurator::new(true);
        for root in &roots {
            configurator.warm_session_workspace(root);
        }
        for root in &roots {
            let code_index = configurator.hostlib.handles_for(root).code_index;
            code_index.wait_until_idle();
            let shared = code_index.shared();
            let live_root = shared
                .lock()
                .unwrap()
                .as_ref()
                .map(|state| state.root.clone());
            assert_eq!(live_root.as_ref(), Some(root));
        }
    }
}
