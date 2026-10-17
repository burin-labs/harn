//! Event routes: snapshot reads and SSE live streams for global,
//! session-scoped, and task-scoped event history, plus the workflow
//! trigger-run projection joined against the action-graph topic.

use super::*;

pub(super) async fn list_events(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let filter = EventFilter::from_query(&query, None, None);
    history_response(&state, &filter)
}

pub(super) async fn list_workflow_trigger_runs(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
) -> Response {
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
) -> Response {
    let filter = EventFilter::from_query(&query, Some(session_id), None);
    history_response(&state, &filter)
}

pub(super) async fn list_task_events(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
) -> Response {
    let filter = EventFilter::from_query(&query, None, Some(task_id));
    history_response(&state, &filter)
}

pub(super) async fn stream_events(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    stream_events_response(state, EventFilter::from_query(&query, None, None), headers).await
}

pub(super) async fn stream_session_events(
    State(state): State<ApiState>,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    stream_events_response(
        state,
        EventFilter::from_query(&query, Some(session_id), None),
        headers,
    )
    .await
}

pub(super) async fn stream_task_events(
    State(state): State<ApiState>,
    AxumPath(task_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    stream_events_response(
        state,
        EventFilter::from_query(&query, None, Some(task_id)),
        headers,
    )
    .await
}

fn cursor_error(message: &'static str) -> Response {
    Sse::new(stream::iter([Ok::<_, Infallible>(
        Event::default()
            .event("error")
            .data(json!({"error":{"code":"cursor_unavailable", "message":message}}).to_string()),
    )]))
    .into_response()
}

async fn stream_events_response(
    state: ApiState,
    mut filter: EventFilter,
    headers: HeaderMap,
) -> Response {
    let mut values = headers.get_all("last-event-id").iter();
    if let Some(value) = values.next() {
        let Ok(cursor) = value.to_str() else {
            return cursor_error("Last-Event-ID must be a valid event cursor");
        };
        if values.next().is_some()
            || cursor.is_empty()
            || filter.after.as_deref().is_some_and(|after| after != cursor)
        {
            return cursor_error("event cursors are empty, duplicated, or disagree");
        }
        filter.after = Some(cursor.to_owned());
    }
    // Subscribe before replay so a committed event cannot fall between them.
    let rx = state.events_tx.subscribe();
    let cursor = filter.after.take();
    let history = match state.history(&filter) {
        Ok(history) => history,
        Err(_) => return task_store::unavailable_response(),
    };
    if cursor
        .as_ref()
        .is_some_and(|cursor| !history.iter().any(|event| &event.id == cursor))
    {
        return cursor_error("event cursor is unknown or no longer available in this stream");
    }
    filter.after = cursor;
    let history: Vec<_> = history
        .into_iter()
        .filter(|event| filter.matches(event))
        .collect();
    // The database and live projections are separate replay snapshots. A
    // higher replayed ID does not imply every earlier event was replayed.
    let replayed = history.iter().map(|event| event.id.clone()).collect();
    let replay = stream::iter(
        history
            .into_iter()
            .map(|event| Ok::<_, Infallible>(sse_event(&event))),
    );
    let live = live_api_event_stream(rx, filter, replayed).map(|event| Ok(sse_event(&event)));
    Sse::new(replay.chain(live))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn history_response(state: &ApiState, filter: &EventFilter) -> Response {
    match state.history(filter) {
        Ok(history) => Json(list_response(history)).into_response(),
        Err(_) => task_store::unavailable_response(),
    }
}

fn live_api_event_stream(
    rx: broadcast::Receiver<ApiEvent>,
    filter: EventFilter,
    replayed: BTreeSet<String>,
) -> impl Stream<Item = ApiEvent> {
    stream::unfold(
        (rx, filter, replayed),
        |(mut rx, filter, replayed)| async move {
            loop {
                match rx.recv().await {
                    Ok(event) if filter.matches(&event) && !replayed.contains(&event.id) => {
                        return Some((event, (rx, filter, replayed)));
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    fn event(id: u64) -> ApiEvent {
        ApiEvent::new(
            format!("event_{id:016x}"),
            "2026-10-11T00:00:00Z".into(),
            "message.created".into(),
            Some("session-one".into()),
            Some("task-one".into()),
            json!({}),
            ResourcePointer {
                object: "task".into(),
                id: "task-one".into(),
            },
        )
    }

    #[tokio::test]
    async fn replay_dedup_keeps_lower_live_ids_and_original_cursor() {
        let (tx, rx) = broadcast::channel(16);
        // The task snapshot ended at 6; the later RAM snapshot saw 8. Both
        // notifications reached this subscriber while replay was assembled.
        let replayed = BTreeSet::from([event(8).id]);
        let filter = EventFilter {
            after: Some(event(6).id),
            ..EventFilter::default()
        };
        assert!(tx.send(event(6)).is_ok());
        assert!(tx.send(event(7)).is_ok());
        assert!(tx.send(event(8)).is_ok());
        assert!(tx.send(event(9)).is_ok());
        let live = live_api_event_stream(rx, filter, replayed);
        futures::pin_mut!(live);
        assert_eq!(live.next().await.unwrap().id, event(7).id);
        assert_eq!(live.next().await.unwrap().id, event(9).id);
        assert!(live.next().now_or_never().is_none());
    }
}
