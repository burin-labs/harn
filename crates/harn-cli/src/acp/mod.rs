use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harn_serve::{AcpRuntimeConfigurator, AcpServerConfig};
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

        let Some(path) = source_path else {
            return Ok(());
        };

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

pub(crate) fn server_config(pipeline: Option<String>) -> AcpServerConfig {
    let extensions = pipeline
        .as_deref()
        .map(Path::new)
        .map(crate::package::load_runtime_extensions)
        .unwrap_or_default();
    AcpServerConfig::new(pipeline)
        .with_runtime_configurator(Arc::new(CliAcpRuntimeConfigurator))
        .with_llm_overrides(extensions.llm, extensions.capabilities)
}

pub(crate) async fn run_acp_server(pipeline: Option<&str>) {
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
    harn_serve::run_acp_server(server_config(pipeline.map(str::to_string))).await;
}

pub(crate) async fn run_acp_channel_server(
    pipeline: Option<String>,
    request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
) {
    harn_serve::run_acp_channel_server(server_config(pipeline), request_rx, response_tx).await;
}
