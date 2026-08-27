pub(super) use crate::llm::content_hash::{stable_redacted_json_hash, stable_redacted_string_hash};

/// Names of the tools the model was actually offered, in served order.
///
/// The hashes below prove two calls served the *same* surface; only the names
/// answer "was the tool this task asked for even on the menu?" — the first
/// question anyone asks when an agent ignores an instruction, and one a
/// persisted run could not previously answer at any cost. A tool name is
/// vocabulary rather than payload, so it is recorded in full.
fn served_tool_names(
    tool_schemas: &[crate::llm::tools::ToolSchema],
    native_tools: &serde_json::Value,
) -> Vec<String> {
    let native = native_tools
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            // Providers disagree on where the name sits: alongside the schema, or
            // nested under the `function` wrapper. Both are the same fact.
            tool.get("name")
                .or_else(|| tool.pointer("/function/name"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    // The two lists usually describe the SAME surface — a native-tool call
    // carries `tool_schema_count == native_tool_count` — so this must dedupe
    // across the whole sequence, not just consecutive pairs the way
    // `Vec::dedup` does. First-seen order is kept because served order is what
    // an operator is reading the list to reconstruct.
    let mut seen = std::collections::HashSet::new();
    tool_schemas
        .iter()
        .map(|schema| schema.name.clone())
        .chain(native)
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

/// Fingerprint the caller's output contract and its provider-compatible
/// projection without retaining either schema in the default transcript.
///
/// `LlmRequestPayload` is the single request boundary that owns provider
/// compatibility projection. Reading the sent hash from it keeps this receipt
/// aligned with streaming validation and provider serialization instead of
/// reconstructing provider policy in observability code.
pub(super) fn structured_output_receipt(
    opts: &super::super::api::LlmCallOptions,
    payload: &super::super::api::LlmRequestPayload,
) -> serde_json::Value {
    let (mode, strict, requested_schema, sent_schema) = match &opts.output_format {
        super::super::api::OutputFormat::Text => ("text", None, None, None),
        super::super::api::OutputFormat::JsonObject => ("json_object", None, None, None),
        super::super::api::OutputFormat::JsonSchema { schema, strict } => (
            "json_schema",
            Some(*strict),
            Some(opts.output_schema.as_ref().unwrap_or(schema)),
            payload.output_schema.as_ref(),
        ),
    };
    let hash = |schema: Option<&serde_json::Value>| {
        schema
            .map(stable_redacted_json_hash)
            .map_or(serde_json::Value::Null, serde_json::Value::String)
    };

    serde_json::json!({
        "schema": "harn.llm.structured_output_receipt.v1",
        "mode": mode,
        "strict": strict,
        "requested_schema_content_hash": hash(requested_schema),
        "sent_schema_content_hash": hash(sent_schema),
    })
}

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
        "served_tool_names": served_tool_names(tool_schemas, &native_tools),
        "native_tools_content_hash": stable_redacted_json_hash(&native_tools),
        "native_tool_count": payload.native_tools.as_ref().map(|tools| tools.len()).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn served_tool_names_reports_each_tool_once_in_served_order() {
        // A native-tool call carries the same surface twice: once as VM tool
        // schemas, once as provider-shaped native tools. Both provider name
        // placements appear here because both are in the wild.
        let schemas = vec![
            crate::llm::tools::ToolSchema {
                name: "look".to_string(),
                description: String::new(),
                params: Vec::new(),
                compact: false,
            },
            crate::llm::tools::ToolSchema {
                name: "dispatch".to_string(),
                description: String::new(),
                params: Vec::new(),
                compact: false,
            },
        ];
        let native = serde_json::json!([
            {"name": "look"},
            {"type": "function", "function": {"name": "dispatch"}},
            {"name": "run"}
        ]);

        assert_eq!(
            served_tool_names(&schemas, &native),
            vec![
                "look".to_string(),
                "dispatch".to_string(),
                "run".to_string()
            ],
            "duplicates across the two lists must collapse even when they are not adjacent"
        );
    }

    #[test]
    fn served_tool_names_is_empty_when_no_tools_were_offered() {
        assert!(served_tool_names(&[], &serde_json::Value::Null).is_empty());
    }

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

    #[test]
    fn provider_request_receipts_distinguish_requested_and_sent_output_schemas() {
        let _guard = crate::llm::env_guard();
        let previous_verbose = set_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", None);
        let dir = temp_transcript_dir("harn-output-schema-receipt");
        let dir_string = dir.to_string_lossy().to_string();
        super::super::push_llm_transcript_dir(&dir_string);

        let requested_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "private_contract_marker": {"type": "string", "maxLength": 5}
            },
            "required": ["private_contract_marker"],
            "additionalProperties": false
        });
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-5.6-luna".to_string();
        opts.output_schema = Some(requested_schema.clone());
        opts.output_format = crate::llm::api::OutputFormat::JsonSchema {
            schema: requested_schema.clone(),
            strict: true,
        };

        let payload =
            super::super::dump_llm_request(1, "call-output-schema-receipt", "none", &opts)
                .expect("valid request");
        super::super::pop_llm_transcript_dir();
        restore_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", previous_verbose);

        let events = read_transcript_events(&dir);
        let request = events
            .iter()
            .find(|event| event["type"] == "provider_call_request")
            .expect("provider_call_request event");
        let receipt = &request["structured_output"];
        let sent_schema = payload.output_schema.expect("sent output schema");

        assert_eq!(receipt["mode"], serde_json::json!("json_schema"));
        assert_eq!(receipt["strict"], serde_json::json!(true));
        assert_eq!(
            receipt["requested_schema_content_hash"],
            serde_json::json!(stable_redacted_json_hash(&requested_schema))
        );
        assert_eq!(
            receipt["sent_schema_content_hash"],
            serde_json::json!(stable_redacted_json_hash(&sent_schema))
        );
        assert_ne!(
            receipt["requested_schema_content_hash"], receipt["sent_schema_content_hash"],
            "the control schema must exercise provider compatibility projection"
        );
        assert!(requested_schema.to_string().contains("maxLength"));
        assert!(
            sent_schema["properties"]["private_contract_marker"]
                .get("maxLength")
                .is_none(),
            "provider schema must omit the unsupported length cap: {sent_schema}"
        );
        assert!(
            !request.to_string().contains("private_contract_marker"),
            "default request events must retain schema hashes, not schema contents"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_free_output_modes_do_not_invent_schema_hashes_or_strictness() {
        for (format, expected_mode) in [
            (crate::llm::api::OutputFormat::Text, "text"),
            (crate::llm::api::OutputFormat::JsonObject, "json_object"),
        ] {
            let mut opts = crate::llm::api::options::base_opts("openai");
            opts.output_format = format;
            let payload = crate::llm::api::LlmRequestPayload::from(&opts);
            let receipt = structured_output_receipt(&opts, &payload);

            assert_eq!(receipt["mode"], serde_json::json!(expected_mode));
            assert!(receipt["strict"].is_null());
            assert!(receipt["requested_schema_content_hash"].is_null());
            assert!(receipt["sent_schema_content_hash"].is_null());
        }
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
