use super::*;

#[test]
fn candidates_from_anthropic_shape() {
    let tools = serde_json::json!([
        {
            "name": "weather_lookup",
            "description": "Look up the weather",
            "input_schema": {
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "City name"}
                }
            }
        }
    ]);
    let got = candidates_from_native(tools.as_array().unwrap());
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "weather_lookup");
    assert!(got[0].description.contains("weather"));
    assert_eq!(got[0].param_text, vec!["city: City name".to_string()]);
}

#[test]
fn candidates_from_openai_shape() {
    let tools = serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "execute_sql",
                "description": "Run a SQL query",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"}
                    }
                }
            }
        }
    ]);
    let got = candidates_from_native(tools.as_array().unwrap());
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "execute_sql");
    assert_eq!(got[0].param_text, vec!["query".to_string()]);
}

#[test]
fn in_tree_runs_bm25_by_default() {
    let candidates = vec![
        ToolCandidate {
            name: "weather".to_string(),
            description: String::new(),
            param_text: Vec::new(),
        },
        ToolCandidate {
            name: "cooking".to_string(),
            description: String::new(),
            param_text: Vec::new(),
        },
    ];
    let outcome = run_in_tree(InTreeStrategy::Bm25, "weather", &candidates, 5);
    assert_eq!(outcome.tool_names, vec!["weather"]);
}

#[test]
fn in_tree_rejects_empty_query() {
    let candidates = vec![ToolCandidate {
        name: "any".to_string(),
        description: String::new(),
        param_text: Vec::new(),
    }];
    let outcome = run_in_tree(InTreeStrategy::Bm25, "   ", &candidates, 5);
    assert!(outcome.tool_names.is_empty());
    assert!(outcome.diagnostic.is_some());
}

#[test]
fn search_outcome_into_tool_result_serializes() {
    let outcome = SearchOutcome {
        tool_names: vec!["a".to_string(), "b".to_string()],
        diagnostic: None,
    };
    let json = outcome.into_tool_result();
    assert_eq!(
        json,
        serde_json::json!({"tool_names": ["a", "b"]}),
        "result shape must be the minimal {{tool_names}} contract"
    );
}

#[test]
fn search_outcome_with_diagnostic() {
    let outcome = SearchOutcome::empty("nothing to see here");
    let json = outcome.into_tool_result();
    assert_eq!(
        json,
        serde_json::json!({"tool_names": [], "diagnostic": "nothing to see here"})
    );
}
