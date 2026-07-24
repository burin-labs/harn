pub(super) fn mutation_status(result: &serde_json::Value) -> &'static str {
    let status = fact(result, "mutation_status").and_then(serde_json::Value::as_str);
    match status {
        Some("applied") => crate::agent_events::ToolMutationStatus::Applied.as_str(),
        Some("not_applied") => crate::agent_events::ToolMutationStatus::NotApplied.as_str(),
        _ => crate::agent_events::ToolMutationStatus::Unknown.as_str(),
    }
}

pub(super) fn changed_paths(result: &serde_json::Value) -> Option<Vec<&str>> {
    let paths = fact(result, "changed_paths")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .collect();
    Some(paths)
}

fn fact<'a>(result: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    result.get(key).or_else(|| {
        if result.get("schema").and_then(serde_json::Value::as_str)
            != Some("harn.agent_tool_handler_result.v1")
        {
            return None;
        }
        result.get("data")?.get(key)
    })
}

#[cfg(test)]
mod tests {
    use super::{changed_paths, mutation_status};

    #[test]
    fn lifts_only_declared_mutation_outcomes() {
        assert_eq!(
            mutation_status(&serde_json::json!({"mutation_status": "applied"})),
            "applied"
        );
        assert_eq!(
            mutation_status(&serde_json::json!({"mutation_status": "not_applied"})),
            "not_applied"
        );
        assert_eq!(
            mutation_status(&serde_json::json!({
                "schema": "harn.agent_tool_handler_result.v1",
                "text": "Edited src/lib.rs",
                "data": {"mutation_status": "applied"}
            })),
            "applied"
        );
        assert_eq!(
            mutation_status(&serde_json::json!({
                "schema": "harn.agent_tool_handler_result.v1",
                "mutation_status": "not_applied",
                "data": {"mutation_status": "applied"}
            })),
            "not_applied"
        );
        for result in [
            serde_json::json!({}),
            serde_json::json!({"mutation_status": "maybe"}),
            serde_json::json!({"mutation_status": 1}),
            serde_json::json!({"mutationStatus": "applied"}),
            serde_json::json!({
                "schema": "another.result.v1",
                "data": {"mutation_status": "applied"}
            }),
        ] {
            assert_eq!(mutation_status(&result), "unknown");
        }
    }

    #[test]
    fn lifts_only_nonempty_string_paths() {
        let result = serde_json::json!({
            "changed_paths": ["src/lib.rs", "", 7, "tests/lib.rs"]
        });
        assert_eq!(
            changed_paths(&result),
            Some(vec!["src/lib.rs", "tests/lib.rs"])
        );
        assert_eq!(
            changed_paths(&serde_json::json!({
                "schema": "harn.agent_tool_handler_result.v1",
                "text": "Edited src/lib.rs",
                "data": {"changed_paths": ["src/lib.rs"]}
            })),
            Some(vec!["src/lib.rs"])
        );
        assert!(changed_paths(&serde_json::json!({
            "changed_paths": "src/lib.rs"
        }))
        .is_none());
    }
}
