use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value as JsonValue;
use tokio::sync::broadcast;

use crate::cli::{McpServeArgs, OrchestratorLocalArgs};
use crate::commands::orchestrator::listener::ListenerAuth;

use super::super::oauth_resource::OAuthResourceServer;
use super::types::{
    AbortOnDrop, LogWatcherReadiness, McpListChangeKind, McpLogNotification,
    McpOrchestratorService, McpResourceNotification, McpTaskNotification,
};
use super::watchers::{
    refresh_manifest_derived_state_cache, send_list_changed, spawn_log_topic_watchers,
    start_list_change_watcher,
};
use harn_serve::FilePromptCatalog;

use super::LOG_NOTIFICATION_CAPACITY;

impl McpOrchestratorService {
    pub(super) fn new(args: &McpServeArgs) -> Result<Self, String> {
        Self::new_local(args.local.clone())
    }

    pub(crate) fn new_local(local: OrchestratorLocalArgs) -> Result<Self, String> {
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
        let prompt_catalog = Arc::new(Mutex::new(FilePromptCatalog::discover(&project_root)));
        let manifest_source = Arc::new(Mutex::new(manifest_source));
        let (list_notify_tx, _) = broadcast::channel(64);
        let (resource_notify_tx, _) = broadcast::channel(128);
        let (task_notify_tx, _) = broadcast::channel(64);
        let (log_notify_tx, _) = broadcast::channel(LOG_NOTIFICATION_CAPACITY);
        let list_watcher = start_list_change_watcher(
            project_root,
            local.config.clone(),
            manifest_source.clone(),
            prompt_catalog.clone(),
            list_notify_tx.clone(),
        );
        let log_watchers_ready = Arc::new(LogWatcherReadiness::default());
        let (log_event_log, log_watchers) = spawn_log_topic_watchers(
            &local.state_dir,
            log_notify_tx.clone(),
            log_watchers_ready.clone(),
        );
        Ok(Self {
            config_path: local.config,
            state_dir: local.state_dir,
            manifest_source,
            auth,
            oauth,
            prompt_catalog,
            list_notify_tx,
            resource_notify_tx,
            task_notify_tx,
            log_notify_tx,
            log_event_log,
            log_watchers_ready,
            tasks: Arc::new(Mutex::new(BTreeMap::new())),
            resource_watchers: Arc::new(Mutex::new(BTreeMap::new())),
            _list_watcher: Arc::new(Mutex::new(list_watcher)),
            _log_watchers: Arc::new(AbortOnDrop(log_watchers)),
        })
    }

    pub(super) fn local_args(&self) -> OrchestratorLocalArgs {
        OrchestratorLocalArgs {
            config: self.config_path.clone(),
            state_dir: self.state_dir.clone(),
        }
    }

    pub(crate) fn notify_manifest_reloaded(&self) {
        if let Ok(manifest_source) = std::fs::read_to_string(&self.config_path) {
            self.refresh_manifest_derived_state(manifest_source);
        }
        self.notify_list_changed(&[
            McpListChangeKind::Tools,
            McpListChangeKind::Resources,
            McpListChangeKind::Prompts,
        ]);
    }

    pub(super) fn refresh_manifest_derived_state(&self, manifest_source: String) {
        let project_root = self
            .config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        refresh_manifest_derived_state_cache(
            &project_root,
            &self.manifest_source,
            &self.prompt_catalog,
            manifest_source,
        );
    }

    pub(super) fn notify_list_changed(&self, kinds: &[McpListChangeKind]) {
        send_list_changed(&self.list_notify_tx, kinds);
    }

    pub(super) fn subscribe_list_notifications(&self) -> broadcast::Receiver<JsonValue> {
        self.list_notify_tx.subscribe()
    }

    pub(super) fn subscribe_resource_notifications(
        &self,
    ) -> broadcast::Receiver<McpResourceNotification> {
        self.resource_notify_tx.subscribe()
    }

    pub(super) fn subscribe_task_notifications(&self) -> broadcast::Receiver<McpTaskNotification> {
        self.task_notify_tx.subscribe()
    }

    pub(super) fn subscribe_log_notifications(&self) -> broadcast::Receiver<McpLogNotification> {
        self.log_notify_tx.subscribe()
    }

    #[cfg(test)]
    pub(super) async fn wait_for_log_watchers_ready(&self) {
        use std::sync::atomic::Ordering;
        loop {
            let notified = self.log_watchers_ready.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let expected = self.log_watchers_ready.expected.load(Ordering::SeqCst);
            if expected > 0 && self.log_watchers_ready.ready.load(Ordering::SeqCst) >= expected {
                return;
            }
            notified.await;
        }
    }
}
