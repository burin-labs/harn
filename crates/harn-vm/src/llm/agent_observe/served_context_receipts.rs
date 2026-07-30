pub(super) fn stable_redacted_string_hash(value: &str) -> String {
    let policy = crate::redact::current_policy();
    let redacted = match policy.redact_json(&serde_json::Value::String(value.to_string())) {
        serde_json::Value::String(redacted) => redacted,
        other => serde_json::to_string(&other).unwrap_or_default(),
    };
    stable_content_hash(redacted.as_bytes())
}

pub(super) fn stable_redacted_json_hash(value: &serde_json::Value) -> String {
    let redacted = crate::redact::current_policy().redact_json(value);
    let encoded = serde_json::to_vec(&redacted).unwrap_or_default();
    stable_content_hash(&encoded)
}

pub(super) fn served_context_receipt(
    opts: &super::super::api::LlmCallOptions,
    tool_schemas: &[crate::llm::tools::ToolSchema],
) -> serde_json::Value {
    let system_prompt = opts.system.as_deref().unwrap_or("");
    let messages = serde_json::Value::Array(opts.messages.clone());
    let tool_schemas_value = serde_json::to_value(tool_schemas).unwrap_or(serde_json::Value::Null);
    let native_tools = opts
        .native_tools
        .as_ref()
        .map(|tools| serde_json::Value::Array(tools.clone()))
        .unwrap_or(serde_json::Value::Null);

    serde_json::json!({
        "schema": "harn.llm.served_context.v1",
        "redaction": "current_policy",
        "system_prompt_content_hash": stable_redacted_string_hash(system_prompt),
        "system_prompt_bytes": system_prompt.len(),
        "messages_content_hash": stable_redacted_json_hash(&messages),
        "message_count": opts.messages.len(),
        "tool_schemas_content_hash": stable_redacted_json_hash(&tool_schemas_value),
        "tool_schema_count": tool_schemas.len(),
        "native_tools_content_hash": stable_redacted_json_hash(&native_tools),
        "native_tool_count": opts.native_tools.as_ref().map(|tools| tools.len()).unwrap_or(0),
    })
}

fn stable_content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_env_for_test(key: &str, value: Option<&str>) -> Option<String> {
        let previous = std::env::var(key).ok();
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        previous
    }

    fn restore_env_for_test(key: &str, previous: Option<String>) {
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    fn temp_transcript_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("{label}-{}", uuid::Uuid::now_v7()))
    }

    fn read_transcript_events(dir: &std::path::Path) -> Vec<serde_json::Value> {
        let transcript =
            std::fs::read_to_string(dir.join("llm_transcript.jsonl")).expect("transcript");
        transcript
            .lines()
            .map(|line| serde_json::from_str(line).expect("transcript event json"))
            .collect()
    }

    #[test]
    fn dump_llm_request_emits_stable_served_context_hashes_without_verbose_snapshot() {
        let _guard = crate::llm::env_guard();
        let previous_verbose = set_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", None);
        let dir = temp_transcript_dir("harn-served-context");
        let dir_string = dir.to_string_lossy().to_string();
        super::super::push_llm_transcript_dir(&dir_string);

        let native_tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Edit a file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path"}
                    },
                    "required": ["path"]
                }
            }
        });
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-test".to_string();
        opts.system = Some("System policy".to_string());
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "fix the bug"}),
            serde_json::json!({"role": "assistant", "content": "I will inspect it"}),
        ];
        opts.native_tools = Some(vec![native_tool.clone()]);

        super::super::dump_llm_request(2, "call-served-context", "native", &opts);

        super::super::pop_llm_transcript_dir();
        restore_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", previous_verbose);

        let events = read_transcript_events(&dir);
        let system = events
            .iter()
            .find(|event| event["type"] == "system_prompt")
            .expect("system_prompt event");
        let schemas = events
            .iter()
            .find(|event| event["type"] == "tool_schemas")
            .expect("tool_schemas event");
        let request = events
            .iter()
            .find(|event| event["type"] == "provider_call_request")
            .expect("provider_call_request event");
        let served_context = &request["served_context"];

        assert!(
            request.get("request_snapshot").is_none(),
            "served context receipts must not require verbose snapshots"
        );
        assert_eq!(
            served_context["schema"],
            serde_json::json!("harn.llm.served_context.v1")
        );
        assert_eq!(served_context["message_count"], serde_json::json!(2));
        assert_eq!(served_context["native_tool_count"], serde_json::json!(1));
        assert_eq!(request["message_count"], serde_json::json!(2));
        assert_eq!(request["native_tool_count"], serde_json::json!(1));
        assert!(
            served_context["tool_schema_count"]
                .as_u64()
                .unwrap_or_default()
                >= 1,
            "provider-declared native tools should appear in normalized tool schemas"
        );

        for value in [
            &system["content_hash"],
            &schemas["content_hash"],
            &served_context["messages_content_hash"],
            &served_context["native_tools_content_hash"],
        ] {
            let hash = value.as_str().expect("stable hash string");
            assert!(
                hash.starts_with("blake3:"),
                "stable content hashes should be explicitly typed: {hash}"
            );
        }
        assert_eq!(
            served_context["system_prompt_content_hash"],
            system["content_hash"]
        );
        assert_eq!(
            served_context["tool_schemas_content_hash"],
            schemas["content_hash"]
        );

        let expected_messages_hash =
            stable_redacted_json_hash(&serde_json::Value::Array(opts.messages));
        let expected_native_tools_hash =
            stable_redacted_json_hash(&serde_json::Value::Array(vec![native_tool]));
        assert_eq!(
            served_context["messages_content_hash"],
            serde_json::json!(expected_messages_hash)
        );
        assert_eq!(
            served_context["native_tools_content_hash"],
            serde_json::json!(expected_native_tools_hash)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every per-call `served_context` hash must resolve to retained bytes
    /// earlier in the SAME transcript, and a reader must be able to re-derive
    /// the hash from those bytes.
    ///
    /// Both halves matter for forensics. The prompt and schema payloads are
    /// deduplicated against the LAST emitted hash, so most calls carry a hash
    /// whose bytes live on an earlier line; if that line is ever missing, the
    /// receipt is a dangling label and blame cannot be assigned to the schema
    /// versus the tool. And because the persisted line is redacted while the
    /// hash is taken over the redacted form, re-hashing the retained bytes must
    /// reproduce the receipt — otherwise a reader cannot prove the bytes it is
    /// reading are the bytes that were served.
    #[test]
    fn served_context_hashes_resolve_to_retained_bytes_across_a_multi_call_run() {
        let _guard = crate::llm::env_guard();
        let previous_verbose = set_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", None);
        let dir = temp_transcript_dir("harn-served-context-join");
        let dir_string = dir.to_string_lossy().to_string();
        super::super::push_llm_transcript_dir(&dir_string);

        let edit_tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Edit a file",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                },
            },
        });
        let read_tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                },
            },
        });

        // The prompt and the schema set each oscillate (A -> B -> A) so
        // deduplication is exercised in both directions: a repeat must be
        // suppressed, and a revert must not resolve to the wrong line.
        let plan = [
            ("policy A", &edit_tool),
            ("policy A", &edit_tool),
            ("policy B", &edit_tool),
            ("policy B", &read_tool),
            ("policy A", &read_tool),
            ("policy A", &edit_tool),
        ];
        for (index, (system, tool)) in plan.iter().enumerate() {
            let mut opts = crate::llm::api::options::base_opts("openai");
            opts.model = "gpt-test".to_string();
            opts.system = Some((*system).to_string());
            opts.messages = vec![serde_json::json!({
                "role": "user",
                "content": format!("turn {index}"),
            })];
            opts.native_tools = Some(vec![(*tool).clone()]);
            super::super::dump_llm_request(index, &format!("call-{index}"), "native", &opts);
        }

        super::super::pop_llm_transcript_dir();
        restore_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", previous_verbose);

        let events = read_transcript_events(&dir);
        let mut retained_prompts: std::collections::BTreeSet<String> = Default::default();
        let mut retained_schemas: std::collections::BTreeSet<String> = Default::default();
        let mut prompt_events = 0usize;
        let mut schema_events = 0usize;
        let mut checked_calls = 0usize;

        for event in &events {
            match event["type"].as_str().unwrap_or_default() {
                "system_prompt" => {
                    let hash = event["content_hash"].as_str().expect("prompt content hash");
                    let content = event["content"].as_str().expect("retained prompt bytes");
                    assert_eq!(
                        stable_redacted_string_hash(content),
                        hash,
                        "retained prompt bytes must re-derive their receipt hash"
                    );
                    retained_prompts.insert(hash.to_string());
                    prompt_events += 1;
                }
                "tool_schemas" => {
                    let hash = event["content_hash"].as_str().expect("schema content hash");
                    let schemas = &event["schemas"];
                    assert!(
                        schemas.is_array(),
                        "served tool schemas must be retained as an array, got {schemas}"
                    );
                    assert_eq!(
                        stable_redacted_json_hash(schemas),
                        hash,
                        "retained tool schemas must re-derive their receipt hash"
                    );
                    retained_schemas.insert(hash.to_string());
                    schema_events += 1;
                }
                "provider_call_request" => {
                    let call_id = event["call_id"].as_str().unwrap_or_default();
                    let served = &event["served_context"];
                    let prompt_hash = served["system_prompt_content_hash"]
                        .as_str()
                        .expect("served prompt hash");
                    let schema_hash = served["tool_schemas_content_hash"]
                        .as_str()
                        .expect("served schema hash");
                    assert!(
                        retained_prompts.contains(prompt_hash),
                        "{call_id} served prompt {prompt_hash} with no retained bytes in the transcript"
                    );
                    assert!(
                        retained_schemas.contains(schema_hash),
                        "{call_id} served schemas {schema_hash} with no retained bytes in the transcript"
                    );
                    checked_calls += 1;
                }
                _ => {}
            }
        }

        assert_eq!(checked_calls, plan.len(), "every call must be attributable");
        // Without suppression the join is trivially satisfied by one payload
        // event per call, so the interesting case would go uncovered.
        assert!(
            prompt_events < plan.len() && schema_events < plan.len(),
            "deduplication must have suppressed repeats \
             (prompts {prompt_events}, schemas {schema_events}, calls {})",
            plan.len()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
