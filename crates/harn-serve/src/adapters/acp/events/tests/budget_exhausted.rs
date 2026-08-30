use super::*;

pub(super) fn fixture_budget_exhausted_event() -> AgentEvent {
    AgentEvent::BudgetExhausted {
        session_id: "session-1".to_string(),
        max_iterations: 8,
        kind: Some("total_cost".to_string()),
        cost_usd: Some(0.69),
        wall_clock_ms: Some(1_500),
        limit: Some("total_cost".to_string()),
        limit_value: serde_json::Number::from_f64(1.25),
        projected_cost_usd: Some(0.75),
        session_cost_usd: Some(0.69),
        projected_input_tokens: Some(12_000),
        projected_output_tokens: Some(4_000),
        projection_basis: Some("observed".to_string()),
        headroom_usd: Some(0.56),
        provider: Some("openai".to_string()),
        model: Some("gpt-5.6-sol".to_string()),
    }
}

pub(super) fn empty_budget_exhausted_event() -> AgentEvent {
    AgentEvent::BudgetExhausted {
        session_id: "session-1".to_string(),
        max_iterations: 3,
        kind: None,
        cost_usd: None,
        wall_clock_ms: None,
        limit: None,
        limit_value: None,
        projected_cost_usd: None,
        session_cost_usd: None,
        projected_input_tokens: None,
        projected_output_tokens: None,
        projection_basis: None,
        headroom_usd: None,
        provider: None,
        model: None,
    }
}
