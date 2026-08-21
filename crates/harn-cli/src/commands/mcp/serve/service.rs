use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::cli::{McpServeArgs, OrchestratorLocalArgs};
use crate::commands::orchestrator::listener::ListenerAuth;

use super::super::oauth_resource::OAuthResourceServer;
use super::derived_state::ManifestDerivedState;
use super::types::McpOrchestratorService;
use super::watchers::start_cache_refresh_watcher;

impl McpOrchestratorService {
    pub(super) fn new(args: &McpServeArgs) -> Result<Self, String> {
        Self::new_local(args.local.clone())
    }

    pub(crate) fn new_local(local: OrchestratorLocalArgs) -> Result<Self, String> {
        harn_vm::initialize_runtime_assets();
        let manifest_source = std::fs::read_to_string(&local.config).map_err(|error| {
            format!(
                "failed to read manifest {}: {error}",
                local.config.display()
            )
        })?;
        let auth = ListenerAuth::from_env(false, None)?;
        let oauth = OAuthResourceServer::from_env()?;
        let project_root = local
            .config
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let derived_state = Arc::new(ManifestDerivedState::discover(
            &project_root,
            manifest_source,
        ));
        let cache_watcher =
            start_cache_refresh_watcher(project_root, local.config.clone(), derived_state.clone());
        Ok(Self {
            config_path: local.config,
            state_dir: local.state_dir,
            derived_state,
            auth,
            oauth,
            orchestrator_event_log: std::sync::OnceLock::new(),
            tasks: Arc::new(Mutex::new(BTreeMap::new())),
            _list_watcher: Arc::new(Mutex::new(cache_watcher)),
        })
    }

    /// The orchestrator's event log, opened once per service.
    ///
    /// Same database `OrchestratorRole::build_vm` installs, reached without
    /// building the VM. Two callers racing the first open both get a working
    /// log; the loser's handle is dropped and every later call returns the
    /// winner, so the service keeps exactly one.
    pub(super) fn orchestrator_event_log(
        &self,
    ) -> Result<Arc<harn_vm::event_log::AnyEventLog>, String> {
        if let Some(log) = self.orchestrator_event_log.get() {
            return Ok(log.clone());
        }
        let config = harn_vm::event_log::EventLogConfig::for_state_root(&self.state_dir)
            .map_err(|error| error.to_string())?;
        let log = harn_vm::event_log::open_event_log(&config).map_err(|error| error.to_string())?;
        Ok(self.orchestrator_event_log.get_or_init(|| log).clone())
    }

    pub(super) fn local_args(&self) -> OrchestratorLocalArgs {
        OrchestratorLocalArgs {
            config: self.config_path.clone(),
            state_dir: self.state_dir.clone(),
        }
    }

    pub(super) fn project_root(&self) -> PathBuf {
        self.config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    }

    pub(super) fn effective_state_dir(&self) -> PathBuf {
        if self.state_dir.is_absolute() {
            self.state_dir.clone()
        } else {
            self.project_root().join(&self.state_dir)
        }
    }

    pub(crate) fn notify_manifest_reloaded(&self) {
        if let Ok(manifest_source) = std::fs::read_to_string(&self.config_path) {
            self.refresh_manifest_derived_state(manifest_source);
        }
    }

    pub(super) fn refresh_manifest_derived_state(&self, manifest_source: String) {
        self.derived_state.refresh(manifest_source);
    }
}
