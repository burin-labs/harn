use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::UnboundedSender;
use futures::StreamExt;
use notify::Watcher;
use serde_json::{json, Value as JsonValue};
use tokio::sync::broadcast;

use harn_vm::event_log::{EventLog, LogEvent, Topic};
use harn_vm::mcp_protocol;

use harn_serve::FilePromptCatalog;

use super::types::{
    HttpSession, LogWatcherReadiness, McpListChangeKind, McpLogNotification, McpLogStreamBinding,
    McpOrchestratorService,
};
use super::util::auth_event_log;
use super::LOG_STREAM_BINDINGS;

pub(super) fn start_list_change_watcher(
    project_root: PathBuf,
    config_path: PathBuf,
    manifest_source_cache: Arc<Mutex<String>>,
    prompt_catalog: Arc<Mutex<FilePromptCatalog>>,
    list_notify_tx: broadcast::Sender<JsonValue>,
) -> Option<notify::RecommendedWatcher> {
    let project_root_for_callback = project_root.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        let prompt_changed = event.paths.iter().any(|path| {
            !is_package_generation_path(path, &project_root_for_callback)
                && is_prompt_reload_path(path)
        });
        let manifest_changed = event.paths.iter().any(|path| {
            !is_package_generation_path(path, &project_root_for_callback)
                && is_manifest_reload_path(path)
        });
        let package_changed = event
            .paths
            .iter()
            .any(|path| is_package_reload_path(path.as_path(), &project_root_for_callback));

        if !prompt_changed && !manifest_changed && !package_changed {
            return;
        }

        if prompt_changed || manifest_changed || package_changed {
            let manifest_source = std::fs::read_to_string(&config_path).unwrap_or_default();
            refresh_manifest_derived_state_cache(
                &project_root_for_callback,
                &manifest_source_cache,
                &prompt_catalog,
                manifest_source,
            );
        }

        let mut kinds = Vec::new();
        if manifest_changed || package_changed {
            kinds.push(McpListChangeKind::Tools);
            kinds.push(McpListChangeKind::Resources);
        }
        if prompt_changed || manifest_changed || package_changed {
            kinds.push(McpListChangeKind::Prompts);
        }
        send_list_changed(&list_notify_tx, &kinds);
    })
    .ok()?;
    watcher
        .watch(&project_root, notify::RecursiveMode::Recursive)
        .ok()?;
    Some(watcher)
}

pub(super) fn refresh_manifest_derived_state_cache(
    project_root: &Path,
    manifest_source_cache: &Arc<Mutex<String>>,
    prompt_catalog: &Arc<Mutex<FilePromptCatalog>>,
    manifest_source: String,
) {
    *manifest_source_cache
        .lock()
        .expect("manifest source poisoned") = manifest_source;
    let updated = FilePromptCatalog::discover(project_root);
    *prompt_catalog.lock().expect("prompt catalog poisoned") = updated;
}

pub(super) fn send_list_changed(
    list_notify_tx: &broadcast::Sender<JsonValue>,
    kinds: &[McpListChangeKind],
) {
    for kind in kinds {
        let _ = list_notify_tx.send(kind.notification());
    }
}

fn is_prompt_reload_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "harn.toml" || name.ends_with(".harn.prompt"))
}

fn is_manifest_reload_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "harn.toml")
}

fn is_package_reload_path(path: &Path, project_root: &Path) -> bool {
    let relative = path.strip_prefix(project_root).unwrap_or(path);
    relative == Path::new(".harn").join("package-current.toml")
}

fn is_package_generation_path(path: &Path, project_root: &Path) -> bool {
    let relative = path.strip_prefix(project_root).unwrap_or(path);
    relative.starts_with(Path::new(".harn").join("package-generations"))
}

pub(super) fn spawn_list_notification_forwarder(
    service: Arc<McpOrchestratorService>,
    sender: UnboundedSender<JsonValue>,
) {
    let mut notifications = service.subscribe_list_notifications();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(message) => {
                    if sender.unbounded_send(message).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

pub(super) fn spawn_resource_notification_forwarder(
    service: Arc<McpOrchestratorService>,
    sender: UnboundedSender<JsonValue>,
    session: Arc<HttpSession>,
) {
    let mut notifications = service.subscribe_resource_notifications();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    let subscribed = session
                        .state
                        .lock()
                        .expect("MCP session poisoned")
                        .subscribed_resources
                        .contains(&notification.uri);
                    if !subscribed {
                        continue;
                    }
                    if sender.unbounded_send(notification.message).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

pub(super) fn spawn_task_notification_forwarder(
    service: Arc<McpOrchestratorService>,
    sender: UnboundedSender<JsonValue>,
    session: Arc<HttpSession>,
) {
    let mut notifications = service.subscribe_task_notifications();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    let owner = session
                        .state
                        .lock()
                        .expect("MCP session poisoned")
                        .client_identity
                        .clone();
                    if notification.owner != owner {
                        continue;
                    }
                    if sender.unbounded_send(notification.message).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

pub(super) fn spawn_log_notification_forwarder(
    service: Arc<McpOrchestratorService>,
    sender: UnboundedSender<JsonValue>,
    session: Arc<HttpSession>,
) {
    let mut notifications = service.subscribe_log_notifications();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    let subscribed_level = session
                        .state
                        .lock()
                        .expect("MCP session poisoned")
                        .log_level;
                    if notification.level < subscribed_level {
                        continue;
                    }
                    if sender.unbounded_send(notification.message).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Open the orchestrator event log and subscribe to each
/// `LOG_STREAM_BINDINGS` topic, fanning new events out as MCP
/// `notifications/message` envelopes on `log_notify_tx`.
///
/// Returns the spawned handles so the service can keep them alive for
/// its lifetime; the watchers terminate when the broadcast sender is
/// dropped.
pub(super) fn spawn_log_topic_watchers(
    state_dir: &Path,
    log_notify_tx: broadcast::Sender<McpLogNotification>,
    readiness: Arc<LogWatcherReadiness>,
) -> (
    Option<Arc<harn_vm::event_log::AnyEventLog>>,
    Vec<tokio::task::JoinHandle<()>>,
) {
    let event_log = match auth_event_log(state_dir) {
        Ok(log) => log,
        Err(error) => {
            eprintln!("[harn] warning: MCP log stream disabled: {error}");
            // Still publish, or a waiter blocks forever on watchers that were
            // never spawned.
            readiness.publish_expected(0);
            return (None, Vec::new());
        }
    };
    let watchers: Vec<_> = LOG_STREAM_BINDINGS
        .iter()
        .filter_map(|binding| {
            spawn_log_topic_watcher(
                event_log.clone(),
                binding,
                log_notify_tx.clone(),
                readiness.clone(),
            )
        })
        .collect();
    readiness.publish_expected(watchers.len());
    (Some(event_log), watchers)
}

fn spawn_log_topic_watcher(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    binding: &'static McpLogStreamBinding,
    log_notify_tx: broadcast::Sender<McpLogNotification>,
    readiness: Arc<LogWatcherReadiness>,
) -> Option<tokio::task::JoinHandle<()>> {
    let topic = match Topic::new(binding.topic) {
        Ok(topic) => topic,
        Err(error) => {
            eprintln!(
                "[harn] warning: MCP log stream skipped invalid topic {}: {error}",
                binding.topic
            );
            return None;
        }
    };
    Some(tokio::spawn(async move {
        // Both failure arms settle before returning: this watcher is spawned,
        // so it is counted in `expected`, and a waiter that never hears from it
        // waits forever.
        let start_from = match event_log.latest(&topic).await {
            Ok(latest) => latest,
            Err(error) => {
                eprintln!(
                    "[harn] warning: MCP log stream cannot read topic {}: {error}",
                    binding.topic
                );
                readiness.record_settled();
                return;
            }
        };
        let mut stream = match event_log.clone().subscribe(&topic, start_from).await {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!(
                    "[harn] warning: MCP log stream cannot subscribe to topic {}: {error}",
                    binding.topic
                );
                readiness.record_settled();
                return;
            }
        };
        readiness.record_settled();
        while let Some(item) = stream.next().await {
            let Ok((event_id, event)) = item else {
                continue;
            };
            let level = severity_for_event(binding, &event);
            let data = json!({
                "event_id": event_id,
                "kind": event.kind,
                "occurred_at_ms": event.occurred_at_ms,
                "headers": event.headers,
                "payload": event.payload,
            });
            let message =
                mcp_protocol::logging_message_notification(level, Some(binding.logger), data);
            if log_notify_tx
                .send(McpLogNotification { level, message })
                .is_err()
            {
                continue;
            }
        }
    }))
}

/// Pick the MCP severity for an event_log entry. Honors an explicit
/// `severity` header when present so producers can opt into a specific
/// level; otherwise heuristics on the event kind elevate failures and
/// errors above the topic's default level.
pub(super) fn severity_for_event(
    binding: &McpLogStreamBinding,
    event: &LogEvent,
) -> mcp_protocol::McpLogLevel {
    if let Some(level) = event
        .headers
        .get("severity")
        .and_then(|value| mcp_protocol::McpLogLevel::from_str_ci(value))
    {
        return level;
    }
    let kind = event.kind.to_ascii_lowercase();
    if kind.contains("error") || kind.contains("panic") {
        return mcp_protocol::McpLogLevel::Error;
    }
    if kind.contains("fail")
        || kind.contains("denied")
        || kind.contains("blocked")
        || kind.contains("rejected")
        || kind.contains("dropped")
        || kind.contains("dlq")
    {
        return mcp_protocol::McpLogLevel::Warning;
    }
    binding.default_level
}

#[cfg(test)]
mod package_reload_tests {
    use super::*;

    #[test]
    fn only_atomic_package_pointer_is_a_package_publication_event() {
        let root = Path::new("workspace");
        assert!(is_package_reload_path(
            Path::new("workspace/.harn/package-current.toml"),
            root
        ));
        assert!(!is_package_reload_path(
            Path::new("workspace/harn.lock"),
            root
        ));
        assert!(!is_package_reload_path(
            Path::new("workspace/.harn/package-generations/generation-a/harn.lock"),
            root
        ));
        assert!(!is_package_reload_path(
            Path::new("workspace/.harn/packages/acme/harn.toml"),
            root
        ));
        assert!(is_package_generation_path(
            Path::new("workspace/.harn/package-generations/generation-a/packages/acme/harn.toml"),
            root
        ));
    }
}
