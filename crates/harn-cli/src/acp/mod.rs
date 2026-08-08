use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harn_serve::{AcpProfileConfig, AcpRuntimeConfigurator, AcpServerConfig, AuthPolicy};
use tokio::sync::mpsc;

struct CliAcpRuntimeConfigurator;

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
            let _ = harn_hostlib::install_default(vm);
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
}

pub(crate) fn server_config(pipeline: Option<String>, auth_policy: AuthPolicy) -> AcpServerConfig {
    let extensions = pipeline
        .as_deref()
        .map(Path::new)
        .map(crate::package::load_runtime_extensions)
        .unwrap_or_default();
    AcpServerConfig::new(pipeline)
        .with_auth_policy(auth_policy)
        .with_runtime_configurator(Arc::new(CliAcpRuntimeConfigurator))
        .with_llm_overrides(extensions.llm, extensions.capabilities)
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
) {
    ensure_acp_event_log(pipeline);
    if trace {
        harn_vm::llm::enable_tracing();
    }
    harn_serve::run_acp_server(
        server_config(pipeline.map(str::to_string), auth_policy).with_profile(profile),
    )
    .await;
    if trace {
        eprint!("{}", crate::commands::run::render_trace_summary());
    }
}

pub(crate) async fn run_acp_channel_server(
    pipeline: Option<String>,
    request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
) {
    harn_serve::run_acp_channel_server(
        server_config(pipeline, AuthPolicy::allow_all()),
        request_rx,
        response_tx,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn configure_fixture(declared: bool) -> Result<(), String> {
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
        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        let result = CliAcpRuntimeConfigurator
            .configure(&mut vm, Some(&script))
            .await;
        harn_vm::reset_thread_local_state();
        result
    }

    #[tokio::test]
    async fn acp_honors_manifest_trusted_host_dispatch_before_installing_triggers() {
        configure_fixture(true)
            .await
            .expect("declared ACP project accepts privileged trigger import graph");

        let error = configure_fixture(false)
            .await
            .expect_err("undeclared ACP project remains unprivileged");
        assert!(
            error.contains("host_call") && error.contains("not callable source API"),
            "unexpected refusal: {error}"
        );
    }
}
