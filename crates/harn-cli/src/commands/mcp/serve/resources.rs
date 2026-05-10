use std::collections::{BTreeMap, BTreeSet};

use futures::StreamExt;
use serde_json::{json, Value as JsonValue};

use harn_vm::event_log::{EventLog, LogEvent, Topic};

use crate::commands::orchestrator::common::{
    load_local_runtime, read_topic, trigger_inspect_dlq, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_DLQ_TOPIC,
    TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_OUTBOX_TOPIC,
};

use super::types::{
    McpOrchestratorService, McpResourceNotification, RecordedTriggerEvent, ResourceSubscription,
};
use super::util::{filter_related_events, preview_events};
use super::{ACTION_GRAPH_TOPIC, TRIGGER_EVENTS_TOPIC};

impl McpOrchestratorService {
    pub(super) async fn handle_resources_list(
        &self,
        id: JsonValue,
        params: &JsonValue,
    ) -> JsonValue {
        match self.list_resources().await {
            Ok(resources) => super::protocol::paginated_list_response(
                id,
                "resources/list",
                "resources",
                params,
                resources,
            ),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32603, &error),
        }
    }

    pub(super) async fn handle_resources_read(
        &self,
        id: JsonValue,
        params: &JsonValue,
    ) -> JsonValue {
        let uri = params
            .get("uri")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        match self.read_resource(uri).await {
            Ok((text, mime_type)) => harn_vm::jsonrpc::response(
                id,
                json!({
                    "contents": [{
                        "uri": uri,
                        "text": text,
                        "mimeType": mime_type,
                    }],
                }),
            ),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32002, &error),
        }
    }

    pub(super) async fn handle_resources_subscribe(
        &self,
        id: JsonValue,
        session: &mut super::types::ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let uri = params
            .get("uri")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let subscription = match self.resource_subscription(uri).await {
            Ok(subscription) => subscription,
            Err(error) => return harn_vm::jsonrpc::error_response(id, -32002, &error),
        };
        session
            .subscribed_resources
            .insert(subscription.uri.clone());
        match self.ensure_resource_update_watcher(subscription).await {
            Ok(()) => harn_vm::jsonrpc::response(id, json!({})),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32603, &error),
        }
    }

    pub(super) fn handle_resources_unsubscribe(
        &self,
        id: JsonValue,
        session: &mut super::types::ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        if let Some(uri) = params.get("uri").and_then(JsonValue::as_str) {
            session.subscribed_resources.remove(uri);
        }
        harn_vm::jsonrpc::response(id, json!({}))
    }

    pub(super) fn notify_topic_resource_changed(&self, topic_name: &str) {
        for uri in resource_uris_for_topic(topic_name) {
            let _ = self.resource_notify_tx.send(McpResourceNotification {
                uri: uri.clone(),
                message: resource_updated_notification(&uri),
            });
        }
    }

    pub(super) async fn list_resources(&self) -> Result<Vec<JsonValue>, String> {
        let mut resources = vec![json!({
            "uri": "harn://manifest",
            "name": "Manifest",
            "description": "The running orchestrator manifest",
            "mimeType": "application/toml",
        })];
        resources.extend(static_topic_resources());

        let ctx = load_local_runtime(&self.local_args()).await?;
        for topic in ctx
            .event_log
            .topics()
            .await
            .map_err(|error| error.to_string())?
        {
            if is_agent_transcript_topic(topic.as_str()) {
                resources.push(topic_resource_def(
                    topic.as_str(),
                    topic.as_str(),
                    "Agent transcript event stream",
                ));
            }
        }
        let recorded = read_topic(&ctx.event_log, TRIGGER_EVENTS_TOPIC).await?;
        for (event_id, event) in recorded {
            let Ok(record) = serde_json::from_value::<RecordedTriggerEvent>(event.payload) else {
                continue;
            };
            resources.push(json!({
                "uri": format!("harn://event/{}", record.event.id.0),
                "name": format!("Event {}", record.event.id.0),
                "description": format!("Trigger event log record #{event_id}"),
                "mimeType": "application/json",
            }));
        }

        let mut ctx = load_local_runtime(&self.local_args()).await?;
        for entry in trigger_inspect_dlq(&mut ctx).await? {
            resources.push(json!({
                "uri": format!("harn://dlq/{}", entry.id),
                "name": format!("DLQ {}", entry.id),
                "description": format!("Pending DLQ entry for event {}", entry.event_id),
                "mimeType": "application/json",
            }));
        }

        Ok(resources)
    }

    pub(super) async fn resource_template_topic_names(&self) -> Result<Vec<String>, String> {
        let mut names = BTreeSet::from([
            "trigger.inbox".to_string(),
            TRIGGER_OUTBOX_TOPIC.to_string(),
        ]);
        let ctx = load_local_runtime(&self.local_args()).await?;
        for topic in ctx
            .event_log
            .topics()
            .await
            .map_err(|error| error.to_string())?
        {
            if is_agent_transcript_topic(topic.as_str()) {
                names.insert(topic.as_str().to_string());
            }
        }
        Ok(names.into_iter().collect())
    }

    pub(super) async fn resource_template_event_ids(&self) -> Result<Vec<String>, String> {
        let ctx = load_local_runtime(&self.local_args()).await?;
        let recorded = read_topic(&ctx.event_log, TRIGGER_EVENTS_TOPIC).await?;
        let mut ids = recorded
            .into_iter()
            .filter_map(|(_, event)| {
                serde_json::from_value::<RecordedTriggerEvent>(event.payload)
                    .ok()
                    .map(|record| record.event.id.0)
            })
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    pub(super) async fn resource_template_dlq_entry_ids(&self) -> Result<Vec<String>, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let mut ids = trigger_inspect_dlq(&mut ctx)
            .await?
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    pub(super) async fn read_resource(&self, uri: &str) -> Result<(String, &'static str), String> {
        if uri == "harn://manifest" {
            return Ok((
                self.manifest_source
                    .lock()
                    .expect("manifest source poisoned")
                    .clone(),
                "application/toml",
            ));
        }
        if let Some(event_id) = uri.strip_prefix("harn://event/") {
            let detail = self.event_resource(event_id).await?;
            return Ok((
                serde_json::to_string_pretty(&detail).map_err(|error| error.to_string())?,
                "application/json",
            ));
        }
        if let Some(entry_id) = uri.strip_prefix("harn://dlq/") {
            let detail = self.dlq_resource(entry_id).await?;
            return Ok((
                serde_json::to_string_pretty(&detail).map_err(|error| error.to_string())?,
                "application/json",
            ));
        }
        if uri.starts_with("harn://topic/") {
            let detail = self.topic_resource(uri).await?;
            return Ok((
                serde_json::to_string_pretty(&detail).map_err(|error| error.to_string())?,
                "application/json",
            ));
        }
        Err(format!("resource not found: {uri}"))
    }

    pub(super) async fn topic_resource(&self, uri: &str) -> Result<JsonValue, String> {
        let subscription = self.resource_subscription(uri).await?;
        let ctx = load_local_runtime(&self.local_args()).await?;
        let events = ctx
            .event_log
            .read_range(&subscription.topic, None, usize::MAX)
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "uri": subscription.uri,
            "topic": subscription.topic.as_str(),
            "events": preview_events(events),
        }))
    }

    pub(super) async fn resource_subscription(
        &self,
        uri: &str,
    ) -> Result<ResourceSubscription, String> {
        let topic_name = topic_name_for_resource_uri(uri)
            .ok_or_else(|| format!("resource is not subscribable: {uri}"))?;
        let topic = Topic::new(topic_name).map_err(|error| error.to_string())?;
        if is_static_subscribable_topic(topic.as_str()) {
            return Ok(ResourceSubscription {
                uri: uri.to_string(),
                topic,
            });
        }

        if is_agent_transcript_topic(topic.as_str()) {
            let ctx = load_local_runtime(&self.local_args()).await?;
            let exists = ctx
                .event_log
                .topics()
                .await
                .map_err(|error| error.to_string())?
                .iter()
                .any(|existing| existing.as_str() == topic.as_str());
            if exists {
                return Ok(ResourceSubscription {
                    uri: uri.to_string(),
                    topic,
                });
            }
        }

        Err(format!("resource not found: {uri}"))
    }

    pub(super) async fn ensure_resource_update_watcher(
        &self,
        subscription: ResourceSubscription,
    ) -> Result<(), String> {
        if self
            .resource_watchers
            .lock()
            .expect("resource watchers poisoned")
            .contains_key(&subscription.uri)
        {
            return Ok(());
        }

        let ctx = load_local_runtime(&self.local_args()).await?;
        let start_from = ctx
            .event_log
            .latest(&subscription.topic)
            .await
            .map_err(|error| error.to_string())?;
        let mut stream = ctx
            .event_log
            .clone()
            .subscribe(&subscription.topic, start_from)
            .await
            .map_err(|error| error.to_string())?;
        let event_log = ctx.event_log.clone();
        let topic = subscription.topic.clone();
        let tx = self.resource_notify_tx.clone();
        let uri = subscription.uri.clone();
        let handle = tokio::spawn(async move {
            let mut last_seen = start_from.unwrap_or(0);
            let mut poll = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                tokio::select! {
                    received = stream.next() => {
                        match received {
                            Some(Ok((event_id, _))) if event_id > last_seen => {
                                last_seen = event_id;
                                let _ = tx.send(McpResourceNotification {
                                    uri: uri.clone(),
                                    message: resource_updated_notification(&uri),
                                });
                            }
                            Some(Ok(_)) => {}
                            Some(Err(_)) | None => break,
                        }
                    }
                    _ = poll.tick() => {
                        match event_log.latest(&topic).await {
                            Ok(Some(event_id)) if event_id > last_seen => {
                                last_seen = event_id;
                                let _ = tx.send(McpResourceNotification {
                                    uri: uri.clone(),
                                    message: resource_updated_notification(&uri),
                                });
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                }
            }
        });

        let mut watchers = self
            .resource_watchers
            .lock()
            .expect("resource watchers poisoned");
        if let std::collections::btree_map::Entry::Vacant(entry) = watchers.entry(subscription.uri)
        {
            entry.insert(handle);
        } else {
            handle.abort();
        }
        Ok(())
    }

    pub(super) async fn event_resource(&self, event_id: &str) -> Result<JsonValue, String> {
        let ctx = load_local_runtime(&self.local_args()).await?;
        let recorded = read_topic(&ctx.event_log, TRIGGER_EVENTS_TOPIC).await?;
        let record = recorded
            .into_iter()
            .find_map(|(log_id, event)| {
                let parsed = serde_json::from_value::<RecordedTriggerEvent>(event.payload).ok()?;
                (parsed.event.id.0 == event_id).then_some((log_id, parsed))
            })
            .ok_or_else(|| format!("unknown trigger event id '{event_id}'"))?;
        let trace_id = record.1.event.trace_id.0.clone();
        let related_outbox = filter_related_events(
            read_topic(&ctx.event_log, TRIGGER_OUTBOX_TOPIC).await?,
            event_id,
            &trace_id,
        );
        let related_attempts = filter_related_events(
            read_topic(&ctx.event_log, TRIGGER_ATTEMPTS_TOPIC).await?,
            event_id,
            &trace_id,
        );
        let related_dlq = filter_related_events(
            read_topic(&ctx.event_log, TRIGGER_DLQ_TOPIC).await?,
            event_id,
            &trace_id,
        );
        let related_graph = filter_related_events(
            read_topic(&ctx.event_log, ACTION_GRAPH_TOPIC).await?,
            event_id,
            &trace_id,
        );
        Ok(json!({
            "log_event_id": record.0,
            "binding_id": record.1.binding_id,
            "binding_version": record.1.binding_version,
            "replay_of_event_id": record.1.replay_of_event_id,
            "event": record.1.event,
            "trace": {
                "trace_id": trace_id,
                "outbox": related_outbox,
                "attempts": related_attempts,
                "dlq": related_dlq,
                "action_graph": related_graph,
            },
        }))
    }

    pub(super) async fn dlq_resource(&self, entry_id: &str) -> Result<JsonValue, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let entry = trigger_inspect_dlq(&mut ctx)
            .await?
            .into_iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| format!("unknown DLQ entry '{entry_id}'"))?;
        serde_json::to_value(entry).map_err(|error| error.to_string())
    }

    pub(super) async fn record_tool_call(
        &self,
        tool_name: &str,
        trace_id: &str,
        client_identity: &str,
        result: &Result<JsonValue, String>,
    ) -> Result<(), String> {
        let status = if result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        let outcome = if result.is_ok() { "success" } else { "error" };

        eprintln!(
            "[harn] mcp: client={} tool={} status={} trace_id={}",
            client_identity, tool_name, status, trace_id
        );

        let ctx = load_local_runtime(&self.local_args()).await?;
        let topic = Topic::new(ACTION_GRAPH_TOPIC).map_err(|error| error.to_string())?;
        let mut headers = BTreeMap::new();
        headers.insert("trace_id".to_string(), trace_id.to_string());
        headers.insert("mcp_client".to_string(), client_identity.to_string());
        headers.insert("tool_name".to_string(), tool_name.to_string());
        let payload = json!({
            "context": {
                "tool_name": tool_name,
                "client_identity": client_identity,
                "trace_id": trace_id,
            },
            "observability": {
                "schema_version": 1,
                "planner_rounds": [],
                "research_fact_count": 0,
                "action_graph_nodes": [{
                    "id": format!("mcp/{trace_id}"),
                    "label": tool_name,
                    "kind": "mcp_tool_call",
                    "status": status,
                    "outcome": outcome,
                    "trace_id": trace_id,
                }],
                "action_graph_edges": [],
                "worker_lineage": [],
                "verification_outcomes": [],
                "transcript_pointers": [],
                "compaction_events": [],
                "daemon_events": [],
            },
            "result": result.as_ref().ok(),
            "error": result.as_ref().err(),
        });
        ctx.event_log
            .append(
                &topic,
                LogEvent::new("action_graph_update", payload).with_headers(headers),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

pub(super) fn static_topic_resources() -> Vec<JsonValue> {
    vec![
        topic_resource_def(
            "trigger.inbox",
            "Trigger Inbox",
            "Queued trigger inbox events",
        ),
        topic_resource_def(
            TRIGGER_OUTBOX_TOPIC,
            "Trigger Outbox",
            "Dispatched trigger outbox events",
        ),
    ]
}

pub(super) fn topic_resource_def(topic_name: &str, name: &str, description: &str) -> JsonValue {
    json!({
        "uri": topic_resource_uri(topic_name),
        "name": name,
        "description": description,
        "mimeType": "application/json",
    })
}

pub(super) fn topic_resource_uri(topic_name: &str) -> String {
    format!("harn://topic/{topic_name}")
}

pub(super) fn topic_name_for_resource_uri(uri: &str) -> Option<&str> {
    let topic_name = uri.strip_prefix("harn://topic/")?;
    match topic_name {
        "trigger.inbox" => Some(TRIGGER_INBOX_ENVELOPES_TOPIC),
        TRIGGER_OUTBOX_TOPIC => Some(TRIGGER_OUTBOX_TOPIC),
        value if is_agent_transcript_topic(value) => Some(value),
        _ => None,
    }
}

pub(super) fn resource_uris_for_topic(topic_name: &str) -> Vec<String> {
    match topic_name {
        TRIGGER_INBOX_ENVELOPES_TOPIC => vec![topic_resource_uri("trigger.inbox")],
        TRIGGER_OUTBOX_TOPIC => vec![topic_resource_uri(TRIGGER_OUTBOX_TOPIC)],
        value if is_agent_transcript_topic(value) => vec![topic_resource_uri(value)],
        _ => Vec::new(),
    }
}

pub(super) fn is_static_subscribable_topic(topic_name: &str) -> bool {
    matches!(
        topic_name,
        TRIGGER_INBOX_ENVELOPES_TOPIC | TRIGGER_OUTBOX_TOPIC
    )
}

pub(super) fn is_agent_transcript_topic(topic_name: &str) -> bool {
    topic_name.starts_with("agent.transcript.")
}

pub(super) fn resource_updated_notification(uri: &str) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/resources/updated",
        "params": { "uri": uri },
    })
}
