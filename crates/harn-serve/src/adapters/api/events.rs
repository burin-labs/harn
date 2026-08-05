//! Event routes: snapshot reads and SSE live streams for global,
//! session-scoped, and task-scoped event history, plus the workflow
//! trigger-run projection joined against the action-graph topic.

use super::*;

pub(super) async fn list_events(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let filter = EventFilter::from_query(&query, None, None);
    Json(list_response(state.history(&filter))).into_response()
}

pub(super) async fn list_workflow_trigger_runs(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let Some(event_log) = state.event_log.clone() else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "event_log_unavailable",
            state
                .event_log_error
                .as_deref()
                .unwrap_or("event log unavailable"),
        );
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let dispatches = match read_api_event_log_topic(&event_log, harn_vm::TRIGGER_OUTBOX_TOPIC).await
    {
        Ok(events) => events,
        Err(message) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "event_log_read_failed",
                &message,
            );
        }
    };
    let action_graph = match read_api_event_log_topic(&event_log, ACTION_GRAPH_TOPIC).await {
        Ok(events) => events,
        Err(message) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "event_log_read_failed",
                &message,
            );
        }
    };
    Json(list_response(workflow_trigger_run_values(
        &dispatches,
        &action_graph,
        limit,
    )))
    .into_response()
}

pub(super) async fn list_session_events(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let filter = EventFilter::from_query(&query, Some(session_id), None);
    Json(list_response(state.history(&filter))).into_response()
}

pub(super) async fn list_task_events(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let filter = EventFilter::from_query(&query, None, Some(task_id));
    Json(list_response(state.history(&filter))).into_response()
}

pub(super) async fn stream_events(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream_events_response(
        state,
        EventFilter::from_query(&query, None, None),
        uri,
        headers,
    )
    .await
}

pub(super) async fn stream_session_events(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream_events_response(
        state,
        EventFilter::from_query(&query, Some(session_id), None),
        uri,
        headers,
    )
    .await
}

pub(super) async fn stream_task_events(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream_events_response(
        state,
        EventFilter::from_query(&query, None, Some(task_id)),
        uri,
        headers,
    )
    .await
}

async fn stream_events_response(
    state: ApiState,
    filter: EventFilter,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let history = state.history(&filter);
    let replay = stream::iter(history.into_iter().map(|event| Ok(sse_event(&event))));
    let live = live_event_stream(state.events_tx.subscribe(), filter);
    Sse::new(replay.chain(live))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn live_event_stream(
    rx: broadcast::Receiver<ApiEvent>,
    filter: EventFilter,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold((rx, filter), |(mut rx, filter)| async move {
        loop {
            match rx.recv().await {
                Ok(event) if filter.matches(&event) => {
                    return Some((Ok(sse_event(&event)), (rx, filter)));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
}

async fn read_api_event_log_topic(
    log: &Arc<AnyEventLog>,
    topic_name: &str,
) -> Result<Vec<(u64, LogEvent)>, String> {
    let topic = Topic::new(topic_name).map_err(|error| error.to_string())?;
    log.read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| error.to_string())
}

fn payload_string(payload: Option<&serde_json::Map<String, Value>>, field: &str) -> Option<String> {
    payload
        .and_then(|payload| payload.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn payload_value(payload: Option<&serde_json::Map<String, Value>>, field: &str) -> Option<Value> {
    payload.and_then(|payload| payload.get(field).cloned())
}

fn workflow_trigger_run_values(
    dispatches: &[(u64, LogEvent)],
    action_graph: &[(u64, LogEvent)],
    limit: usize,
) -> Vec<Value> {
    let graph_by_event_id = action_graph_by_event_id(action_graph);
    let mut recent: Vec<_> = dispatches
        .iter()
        .filter_map(|(event_log_id, event)| {
            if !matches!(
                event.kind.as_str(),
                "dispatch_succeeded" | "dispatch_failed" | "dispatch_skipped"
            ) {
                return None;
            }
            let payload = event.payload.as_object();
            let event_id = event.headers.get("event_id").cloned();
            let kind = event.kind.clone();
            let status = event
                .kind
                .strip_prefix("dispatch_")
                .unwrap_or(event.kind.as_str())
                .to_string();
            let action_graph = event_id
                .as_deref()
                .and_then(|id| graph_by_event_id.get(id))
                .cloned()
                .unwrap_or(Value::Null);
            Some(json!({
                "id": format!("workflow_trigger_run_{event_log_id}"),
                "object": "workflow_trigger_run",
                "event_log_id": event_log_id,
                "kind": kind,
                "status": status,
                "occurred_at_ms": event.occurred_at_ms,
                "trigger_id": event.headers.get("trigger_id").cloned(),
                "event_id": event_id,
                "binding_key": event.headers.get("binding_key").cloned(),
                "attempt": event.headers.get("attempt").and_then(|attempt| attempt.parse::<u32>().ok()),
                "replay_of_event_id": event.headers.get("replay_of_event_id").cloned(),
                "handler_kind": payload_string(payload, "handler_kind"),
                "target_uri": payload_string(payload, "target_uri"),
                "error": payload_string(payload, "error"),
                "result": payload_value(payload, "result"),
                "skip_stage": payload_string(payload, "skip_stage"),
                "detail": payload_value(payload, "detail"),
                "action_graph": action_graph,
            }))
        })
        .collect();

    recent.sort_by_key(|value| {
        value
            .get("occurred_at_ms")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    });
    if recent.len() > limit {
        recent.drain(0..recent.len() - limit);
    }
    recent.reverse();
    recent
}

fn action_graph_by_event_id(events: &[(u64, LogEvent)]) -> BTreeMap<String, Value> {
    let mut by_event_id = BTreeMap::<String, (Vec<Value>, Vec<Value>)>::new();
    for (_, event) in events {
        let Some(event_id) = event.headers.get("event_id").cloned() else {
            continue;
        };
        let entry = by_event_id.entry(event_id).or_default();
        if let Some(nodes) = event
            .payload
            .pointer("/observability/action_graph_nodes")
            .and_then(Value::as_array)
        {
            entry.0.extend(nodes.iter().cloned());
        }
        if let Some(edges) = event
            .payload
            .pointer("/observability/action_graph_edges")
            .and_then(Value::as_array)
        {
            entry.1.extend(edges.iter().cloned());
        }
    }
    by_event_id
        .into_iter()
        .map(|(event_id, (nodes, edges))| {
            (
                event_id,
                json!({
                    "nodes": nodes,
                    "edges": edges,
                }),
            )
        })
        .collect()
}

fn sse_event(event: &ApiEvent) -> Event {
    Event::default()
        .id(event.id.clone())
        .event(event.event.clone())
        .json_data(event)
        .unwrap_or_else(|_| Event::default().event("error").data("{}"))
}
