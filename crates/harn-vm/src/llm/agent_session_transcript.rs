pub(super) fn append_finalized_marker(
    session_id: &str,
    final_status: &str,
    stop_reason: &str,
    iterations: i64,
) {
    let mut fields = serde_json::Map::new();
    fields.insert("session_id".to_string(), serde_json::json!(session_id));
    fields.insert("final_status".to_string(), serde_json::json!(final_status));
    fields.insert("stop_reason".to_string(), serde_json::json!(stop_reason));
    fields.insert("iterations".to_string(), serde_json::json!(iterations));
    fields.insert("terminal".to_string(), serde_json::json!(true));
    super::agent_observe::append_llm_observability_entry("agent_session_finalized", fields);
}
