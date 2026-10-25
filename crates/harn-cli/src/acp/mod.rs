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

/// Hostlib handles for the workspace this server last opened, so a session
/// warm fills the code index that later VMs over that workspace install.
///
/// One slot, as the VM baseline cache holds one: a server that moves to
/// another workspace starts fresh handles instead of keeping an index per
/// root, and VMs already built keep the handles they were built with.
#[cfg(feature = "hostlib")]
#[derive(Default)]
struct WorkspaceHostlib {
    slot: std::sync::Mutex<Option<(std::path::PathBuf, harn_hostlib::DefaultHostlibHandles)>>,
}

#[cfg(feature = "hostlib")]
impl WorkspaceHostlib {
    fn handles_for(&self, project_root: &Path) -> harn_hostlib::DefaultHostlibHandles {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((root, handles)) = slot.as_ref() {
            if root == project_root {
                return handles.clone();
            }
        }
        let handles = crate::default_hostlib_handles();
        *slot = Some((project_root.to_path_buf(), handles.clone()));
        handles
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
        if !self.warm_code_index {
            return;
        }
        #[cfg(feature = "hostlib")]
        {
            let outcome = self
                .hostlib
                .handles_for(project_root)
                .code_index
                .warm_session(project_root);
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
            eprintln!(
                "[harn] ACP session replay disabled: failed to initialize EventLog for {}: {error}",
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
    confinement: Option<harn_serve::AcpServerConfinement>,
) -> Result<(), String> {
    ensure_acp_event_log(pipeline);
    if trace {
        harn_vm::llm::enable_tracing();
    }
    let config = server_config(pipeline.map(str::to_string), auth_policy)?
        .with_profile(profile)
        .with_sandbox(sandbox);
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
}
