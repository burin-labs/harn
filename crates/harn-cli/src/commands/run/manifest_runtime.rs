use std::fmt;
use std::path::Path;

use crate::commands::mcp::{self, AuthResolution};
use crate::package;

pub(crate) struct ManifestRuntimeSetupError {
    phase: &'static str,
    label: &'static str,
    message: String,
}

impl ManifestRuntimeSetupError {
    fn triggers(error: impl fmt::Display) -> Self {
        Self {
            phase: "manifest_triggers",
            label: "manifest triggers",
            message: error.to_string(),
        }
    }

    fn hooks(error: impl fmt::Display) -> Self {
        Self {
            phase: "manifest_hooks",
            label: "manifest hooks",
            message: error.to_string(),
        }
    }

    pub(crate) fn phase(&self) -> &'static str {
        self.phase
    }

    pub(crate) fn label(&self) -> &'static str {
        self.label
    }
}

impl fmt::Display for ManifestRuntimeSetupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Install runtime extensions declared by the entry package.
///
/// An explicit builtin policy targets the entry pipeline, so callers can defer
/// unrelated manifest handler module initialization until those handlers fire.
pub(crate) async fn install_manifest_runtime(
    path: &Path,
    vm: &mut harn_vm::Vm,
    defer_handlers: bool,
) -> Result<(), ManifestRuntimeSetupError> {
    let extensions = package::load_runtime_extensions(path);
    package::install_runtime_extensions(&extensions);
    if let Some(manifest) = extensions.root_manifest.as_ref() {
        if !manifest.mcp.is_empty() {
            connect_mcp_servers(&manifest.mcp, vm).await;
        }
    }
    package::install_manifest_triggers_with_mode(vm, &extensions, defer_handlers)
        .await
        .map_err(ManifestRuntimeSetupError::triggers)?;
    package::install_manifest_hooks_with_mode(vm, &extensions, defer_handlers)
        .await
        .map_err(ManifestRuntimeSetupError::hooks)?;
    Ok(())
}

/// Connect to MCP servers declared in `harn.toml` and register them as
/// `mcp.<name>` globals on the VM. Connection failures are warned but do not
/// abort execution.
///
/// Servers with `lazy = true` are registered with the VM-side MCP registry but
/// are not booted until a skill requires them or user code activates one.
pub(crate) async fn connect_mcp_servers(
    servers: &[package::McpServerConfig],
    vm: &mut harn_vm::Vm,
) {
    use std::collections::BTreeMap;
    use std::time::Duration;

    let mut mcp_dict: BTreeMap<String, harn_vm::VmValue> = BTreeMap::new();
    let mut registrations: Vec<harn_vm::RegisteredMcpServer> = Vec::new();

    for server in servers {
        let resolved_auth = match mcp::resolve_auth_for_server(server).await {
            Ok(resolution) => resolution,
            Err(error) => {
                eprintln!(
                    "warning: mcp: failed to load auth for '{}': {}",
                    server.name, error
                );
                AuthResolution::None
            }
        };
        let spec = serde_json::json!({
            "name": server.name,
            "transport": server.transport.clone().unwrap_or_else(|| "stdio".to_string()),
            "command": server.command,
            "args": server.args,
            "env": server.env,
            "url": server.url,
            "auth_token": match resolved_auth {
                AuthResolution::StaticBearer(token) => Some(token),
                AuthResolution::OAuthStore => None,
                AuthResolution::None => server.auth_token.clone(),
            },
            "token_exchange": server.token_exchange.clone(),
            "protocol_version": server.protocol_version,
            "protocol_mode": server.protocol_mode,
            "proxy_server_name": server.proxy_server_name,
        });

        // Register every server so skill activation and mcp_ensure_active can
        // find lazy entries; eager entries are connected below.
        registrations.push(harn_vm::RegisteredMcpServer {
            name: server.name.clone(),
            spec: spec.clone(),
            lazy: server.lazy,
            card: server.card.clone(),
            keep_alive: server.keep_alive_ms.map(Duration::from_millis),
        });

        if server.lazy {
            eprintln!(
                "[harn] mcp: deferred '{}' (lazy, boots on first use)",
                server.name
            );
            continue;
        }

        match harn_vm::connect_mcp_server_from_json(&spec).await {
            Ok(handle) => {
                eprintln!("[harn] mcp: connected to '{}'", server.name);
                harn_vm::mcp_install_active(&server.name, handle.clone());
                mcp_dict.insert(server.name.clone(), harn_vm::VmValue::mcp_client(handle));
            }
            Err(error) => {
                eprintln!(
                    "warning: mcp: failed to connect to '{}': {}",
                    server.name, error
                );
            }
        }
    }

    // Install registrations after eager connects so the active handles survive.
    harn_vm::mcp_register_servers(registrations);

    if !mcp_dict.is_empty() {
        vm.set_global("mcp", harn_vm::VmValue::dict(mcp_dict));
    }
}
