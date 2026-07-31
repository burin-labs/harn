pub(super) use crate::llm::content_hash::{stable_redacted_json_hash, stable_redacted_string_hash};

pub(super) fn served_context_receipt(
    payload: &super::super::api::LlmRequestPayload,
    manifest: &crate::llm::prompt::ContextAssemblyManifest,
    tool_schemas: &[crate::llm::tools::ToolSchema],
) -> serde_json::Value {
    let system_prompt = payload.system.as_deref().unwrap_or("");
    let messages = serde_json::Value::Array(payload.messages.clone());
    let tool_schemas_value = serde_json::to_value(tool_schemas).unwrap_or(serde_json::Value::Null);
    let native_tools = payload
        .native_tools
        .as_ref()
        .map(|tools| serde_json::Value::Array(tools.clone()))
        .unwrap_or(serde_json::Value::Null);
    let manifest_value = manifest.as_json();

    serde_json::json!({
        "schema": "harn.llm.served_context.v1",
        "redaction": "current_policy",
        "call_role": manifest.call_role(),
        "actor_chain": manifest.actor_chain(),
        "manifest_content_hash": stable_redacted_json_hash(&manifest_value),
        "system_prompt_content_hash": stable_redacted_string_hash(system_prompt),
        "system_prompt_bytes": system_prompt.len(),
        "messages_content_hash": stable_redacted_json_hash(&messages),
        "message_count": payload.messages.len(),
        "tool_schemas_content_hash": stable_redacted_json_hash(&tool_schemas_value),
        "tool_schema_count": tool_schemas.len(),
        "native_tools_content_hash": stable_redacted_json_hash(&native_tools),
        "native_tool_count": payload.native_tools.as_ref().map(|tools| tools.len()).unwrap_or(0),
    })
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
        opts.context_manifest = crate::llm::prompt::ContextAssemblyManifest::internal(
            "test:system",
            "test",
            "llm_call",
            opts.system.as_deref(),
        );
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "fix the bug"}),
            serde_json::json!({"role": "assistant", "content": "I will inspect it"}),
        ];
        opts.native_tools = Some(vec![native_tool.clone()]);

        super::super::dump_llm_request(2, "call-served-context", "native", &opts)
            .expect("valid context manifest");

        super::super::pop_llm_transcript_dir();
        restore_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", previous_verbose);

        let events = read_transcript_events(&dir);
        let system = events
            .iter()
            .find(|event| event["type"] == "system_prompt")
            .expect("system_prompt event");
        let manifest_event = events
            .iter()
            .find(|event| event["type"] == "context_manifest")
            .expect("context_manifest event");
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
        assert_eq!(request["call_role"], serde_json::json!("llm_call"));
        assert_eq!(served_context["call_role"], request["call_role"]);
        assert_eq!(
            served_context["manifest_content_hash"],
            manifest_event["content_hash"]
        );
        assert_eq!(
            stable_redacted_json_hash(&manifest_event["manifest"]),
            manifest_event["content_hash"]
        );
        assert_eq!(
            manifest_event["manifest"]["whole_prompt_digest"],
            served_context["system_prompt_content_hash"]
        );
        assert_eq!(
            manifest_event["manifest"]["system_prompt_bytes"],
            served_context["system_prompt_bytes"]
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
            &manifest_event["content_hash"],
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
            opts.context_manifest = crate::llm::prompt::ContextAssemblyManifest::internal(
                format!("test:system:{system}"),
                "test",
                "llm_call",
                opts.system.as_deref(),
            );
            opts.messages = vec![serde_json::json!({
                "role": "user",
                "content": format!("turn {index}"),
            })];
            opts.native_tools = Some(vec![(*tool).clone()]);
            super::super::dump_llm_request(index, &format!("call-{index}"), "native", &opts)
                .expect("valid context manifest");
        }

        super::super::pop_llm_transcript_dir();
        restore_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", previous_verbose);

        let events = read_transcript_events(&dir);
        let mut retained_prompts: std::collections::BTreeSet<String> = Default::default();
        let mut retained_manifests: std::collections::BTreeSet<String> = Default::default();
        let mut retained_schemas: std::collections::BTreeSet<String> = Default::default();
        let mut prompt_events = 0usize;
        let mut manifest_events = 0usize;
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
                "context_manifest" => {
                    let hash = event["content_hash"]
                        .as_str()
                        .expect("manifest content hash");
                    let manifest = &event["manifest"];
                    assert_eq!(
                        stable_redacted_json_hash(manifest),
                        hash,
                        "retained context manifest must re-derive its receipt hash"
                    );
                    retained_manifests.insert(hash.to_string());
                    manifest_events += 1;
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
                    let manifest_hash = served["manifest_content_hash"]
                        .as_str()
                        .expect("served manifest hash");
                    assert!(
                        retained_prompts.contains(prompt_hash),
                        "{call_id} served prompt {prompt_hash} with no retained bytes in the transcript"
                    );
                    assert!(
                        retained_schemas.contains(schema_hash),
                        "{call_id} served schemas {schema_hash} with no retained bytes in the transcript"
                    );
                    assert!(
                        retained_manifests.contains(manifest_hash),
                        "{call_id} served context manifest {manifest_hash} with no retained bytes in the transcript"
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
            prompt_events < plan.len()
                && manifest_events < plan.len()
                && schema_events < plan.len(),
            "deduplication must have suppressed repeats \
             (prompts {prompt_events}, manifests {manifest_events}, \
              schemas {schema_events}, calls {})",
            plan.len()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_context_manifest_hard_fails_before_any_request_is_retained() {
        let _guard = crate::llm::env_guard();
        let dir = temp_transcript_dir("harn-stale-context-manifest");
        let dir_string = dir.to_string_lossy().to_string();
        super::super::push_llm_transcript_dir(&dir_string);

        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.system = Some("assembled bytes".to_string());
        opts.context_manifest = crate::llm::prompt::ContextAssemblyManifest::internal(
            "test:system",
            "test",
            "llm_call",
            opts.system.as_deref(),
        );
        opts.system = Some("mutated after assembly".to_string());

        let error = super::super::dump_llm_request(0, "call-stale", "native", &opts)
            .expect_err("stale manifest must fail closed");
        super::super::pop_llm_transcript_dir();

        assert!(
            error
                .to_string()
                .contains("context assembly manifest validation failed"),
            "unexpected error: {error}"
        );
        assert!(
            !dir.join("llm_transcript.jsonl").exists(),
            "validation must happen before retaining a provider request"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn served_context_certifies_qwen_thinking_disable_egress_bytes() {
        let _guard = crate::llm::env_guard();
        let dir = temp_transcript_dir("harn-qwen-egress-served-context");
        let dir_string = dir.to_string_lossy().to_string();
        super::super::push_llm_transcript_dir(&dir_string);

        let mut opts = crate::llm::api::options::base_opts("ollama");
        opts.model = "qwen3.5:30b".to_string();
        opts.system = Some("you are an agent.".to_string());
        opts.thinking = crate::llm::api::ThinkingConfig::Disabled;
        opts.context_manifest = crate::llm::prompt::ContextAssemblyManifest::internal(
            "test:system",
            "test",
            "llm_call",
            opts.system.as_deref(),
        );
        let payload = crate::llm::api::LlmRequestPayload::from(&opts);
        assert_eq!(
            payload.system.as_deref(),
            Some("/no_think\nyou are an agent."),
            "the fixture must reach the capability-owned egress transform",
        );

        let observed_payload = super::super::dump_llm_request(0, "call-qwen-egress", "text", &opts)
            .expect("valid context manifest");
        super::super::pop_llm_transcript_dir();
        assert_eq!(
            observed_payload.system, payload.system,
            "transport must receive the payload whose bytes were observed",
        );

        let events = read_transcript_events(&dir);
        let manifest = &events
            .iter()
            .find(|event| event["type"] == "context_manifest")
            .expect("context_manifest event")["manifest"];
        let request = events
            .iter()
            .find(|event| event["type"] == "provider_call_request")
            .expect("provider_call_request event");
        let served_context = &request["served_context"];
        let served_system = payload.system.as_deref().unwrap_or("");
        assert_eq!(
            manifest["boundary"],
            serde_json::json!("llm_request_payload_egress")
        );
        assert_eq!(manifest["root"], serde_json::json!("transformed"));
        assert_eq!(
            manifest["whole_prompt_digest"],
            served_context["system_prompt_content_hash"]
        );
        assert_eq!(
            manifest["system_prompt_bytes"],
            served_context["system_prompt_bytes"]
        );
        assert_eq!(
            manifest["segments"]
                .as_array()
                .expect("manifest segments")
                .iter()
                .find(|segment| segment["included"] == serde_json::json!(true))
                .expect("included egress segment")["id"],
            serde_json::json!("egress:system")
        );
        assert_eq!(
            manifest["egress_delta"]["input_system_prompt_bytes"],
            serde_json::json!(17)
        );
        assert_eq!(
            manifest["egress_delta"]["output_system_prompt_bytes"],
            serde_json::json!(27)
        );
        assert_eq!(
            manifest["egress_delta"]["bytes_added"],
            serde_json::json!(10)
        );
        assert_eq!(
            served_context["system_prompt_bytes"],
            serde_json::json!(served_system.len()),
            "receipt bytes must describe the post-conversion payload",
        );
        assert_eq!(
            served_context["system_prompt_content_hash"],
            serde_json::json!(stable_redacted_string_hash(served_system)),
            "receipt digest must describe the post-conversion payload",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
