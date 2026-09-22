//! Prepare an internal request over explicit data without carrying the active
//! conversation or its tool authority into that request.

use super::{LlmCallOptions, OutputFormat};

impl LlmCallOptions {
    /// Retain admitted routing, credentials, privacy, transport, and budget
    /// settings. Replace the conversation and all fields that can supply
    /// hidden conversation state or executable tools. The caller installs its
    /// own output contract after this boundary.
    pub(crate) fn isolated_request(&self, prompt: String, role: &str, stage: &str) -> Self {
        let mut options = self.clone();
        options.messages = vec![serde_json::json!({"role": "user", "content": prompt})];
        options.system = None;
        options.system_prompt_root = Default::default();
        options.context_manifest = Default::default();
        options.context_manifest.record_system_transform(
            role,
            "stdlib:isolated_request",
            "replaced conversation",
            None,
        );
        options.message_lineage = None;
        options.transcript_summary = None;
        options.reminders = None;
        options.reminder_lifecycle.clear();
        options.tools = None;
        options.native_tools = None;
        options.provider_tools.clear();
        options.tool_choice = None;
        options.parallel_tool_calls = None;
        options.tool_search = None;
        options.max_tool_calls = None;
        options.previous_response_id = None;
        options.prefill = None;
        options.prediction = None;
        options.stop = None;
        options.background = None;
        options.truncation = None;
        options.compact = None;
        // Arbitrary provider-body overrides can carry messages or tools. The
        // typed data_controls field above remains the privacy authority.
        options.provider_overrides = None;
        options.structural_experiment = None;
        options.applied_structural_experiment = None;
        options.output_format = OutputFormat::Text;
        options.output_schema = None;
        options.output_validation = None;
        options.schema_stream_abort = false;
        options.set_call_attribution(role, stage);
        options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_request_replaces_context_but_preserves_authority() {
        let active = LlmCallOptions {
            provider: "mock".into(),
            model: "mock".into(),
            api_key: "explicit test credential".into(),
            region: Some("test-region".into()),
            messages: vec![serde_json::json!({"role": "user", "content": "unselected history"})],
            system: Some("active conversation instructions".into()),
            transcript_summary: Some("older context".into()),
            tools: Some(crate::value::VmValue::string("active tool authority")),
            native_tools: Some(vec![serde_json::json!({"name": "mutate"})]),
            provider_tools: vec![serde_json::json!({"type": "web_search"})],
            previous_response_id: Some("previous-provider-conversation".into()),
            prefill: Some("continue the old answer".into()),
            provider_overrides: Some(serde_json::json!({"tools": ["injected"]})),
            budget: Some(crate::llm::cost::LlmBudgetEnvelope {
                max_cost_usd: Some(0.01),
                ..Default::default()
            }),
            ..Default::default()
        };
        let isolated =
            active.isolated_request("selected source only".into(), "compaction", "compact");
        assert_eq!(
            isolated.messages,
            vec![serde_json::json!({"role": "user", "content": "selected source only"})]
        );
        assert!(isolated.system.is_none() && isolated.transcript_summary.is_none());
        assert!(isolated.tools.is_none() && isolated.native_tools.is_none());
        assert!(isolated.provider_tools.is_empty() && isolated.provider_overrides.is_none());
        assert!(isolated.previous_response_id.is_none() && isolated.prefill.is_none());
        assert_eq!(isolated.provider, active.provider);
        assert_eq!(isolated.model, active.model);
        assert_eq!(isolated.api_key, active.api_key);
        assert_eq!(isolated.region, active.region);
        assert_eq!(isolated.budget, active.budget);
        assert_eq!(active.messages[0]["content"], "unselected history");
    }
}
