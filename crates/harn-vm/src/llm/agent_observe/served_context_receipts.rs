use super::chrono_now;
pub(super) use crate::llm::content_hash::{stable_redacted_json_hash, stable_redacted_string_hash};

use serde::Serialize;

const MESSAGE_LINEAGE_SCHEMA: &str = "harn.llm.message_lineage.v1";
const SERVED_MESSAGE_SCHEMA: &str = "harn.llm.served_message.v1";

#[derive(Clone, Debug, Serialize)]
struct ServedMessageLineage {
    position: usize,
    role: String,
    semantic_kind: crate::llm::message_lineage::MessageSemanticKind,
    message_content_hash: String,
    definition_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_message_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction_receipt_ref: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct MessageLineageManifest {
    schema: &'static str,
    call_id: String,
    iteration: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    projection: crate::llm::message_lineage::ProjectionLineage,
    messages: Vec<ServedMessageLineage>,
}

fn message_role(message: &serde_json::Value) -> String {
    message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn message_lineage_manifest(
    messages: &[serde_json::Value],
    lineage: Option<&crate::llm::message_lineage::MessageLineageManifest>,
    iteration: usize,
    call_id: &str,
    session_id: Option<&str>,
) -> MessageLineageManifest {
    // Egress may structurally remove conversation contributors (for example,
    // a replacement system root). Never shift surviving positions onto the
    // wrong source entry; an unaligned manifest is less truthful than unknown.
    let lineage = lineage.filter(|lineage| lineage.messages.len() == messages.len());
    let messages = messages
        .iter()
        .enumerate()
        .map(|(position, message)| {
            let entry = lineage
                .and_then(|lineage| lineage.messages.get(position))
                .cloned()
                .unwrap_or_default();
            let message_content_hash = stable_redacted_json_hash(message);
            ServedMessageLineage {
                position,
                role: message_role(message),
                semantic_kind: entry.semantic_kind,
                definition_ref: message_content_hash.clone(),
                message_content_hash,
                source_message_index: entry.source_message_index,
                compaction_receipt_ref: entry.compaction_receipt_ref,
            }
        })
        .collect();
    MessageLineageManifest {
        schema: MESSAGE_LINEAGE_SCHEMA,
        call_id: call_id.to_string(),
        iteration,
        session_id: session_id.map(str::to_string),
        projection: lineage
            .map(|lineage| lineage.projection.clone())
            .unwrap_or_else(crate::llm::message_lineage::unknown_projection),
        messages,
    }
}

pub(super) fn served_message_definitions(messages: &[serde_json::Value]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|message| {
            let content_hash = stable_redacted_json_hash(message);
            serde_json::json!({
                "type": "served_message",
                "timestamp": chrono_now(),
                "span_id": crate::tracing::current_span_id(),
                "schema": SERVED_MESSAGE_SCHEMA,
                "content_hash": content_hash,
                "message": message,
            })
        })
        .collect()
}

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
    opts: &super::super::api::LlmCallOptions,
    iteration: usize,
    call_id: &str,
    session_id: Option<&str>,
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
    let message_lineage = message_lineage_manifest(
        &payload.messages,
        payload.aligned_message_lineage(opts),
        iteration,
        call_id,
        session_id,
    );

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
        "message_lineage": message_lineage,
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
                summary_only: false,
            },
            crate::llm::tools::ToolSchema {
                name: "dispatch".to_string(),
                description: String::new(),
                params: Vec::new(),
                summary_only: false,
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
                "content": "same served message",
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
        let mut served_message_events = 0usize;
        let mut retained_messages: std::collections::BTreeSet<String> = Default::default();
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
                "served_message" => {
                    let hash = event["content_hash"]
                        .as_str()
                        .expect("message content hash");
                    assert_eq!(
                        stable_redacted_json_hash(&event["message"]),
                        hash,
                        "retained message bytes must re-derive their receipt hash"
                    );
                    retained_messages.insert(hash.to_string());
                    served_message_events += 1;
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
                    for message in served["message_lineage"]["messages"]
                        .as_array()
                        .expect("ordered message lineage")
                    {
                        let definition_ref = message["definition_ref"]
                            .as_str()
                            .expect("message definition ref");
                        assert!(
                            retained_messages.contains(definition_ref),
                            "{call_id} served message {definition_ref} with no retained definition"
                        );
                    }
                    checked_calls += 1;
                }
                _ => {}
            }
        }

        assert_eq!(checked_calls, plan.len(), "every call must be attributable");
        assert_eq!(
            served_message_events, 1,
            "identical message definitions must be retained once across all calls"
        );
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

    #[test]
    fn raw_resume_does_not_inherit_a_stale_projected_turn() {
        let session_id = crate::agent_sessions::open_or_create_for_test(Some(format!(
            "served-context-raw-resume-{}",
            uuid::Uuid::now_v7()
        )));
        let stale_projection = crate::llm::helpers::transcript_event_with_id(
            "stale-projection",
            "transcript.projection",
            "system",
            "internal",
            "",
            Some(serde_json::json!({
                "policy": "summary_prefix",
                "prefix_hash": "sha256:stale",
            })),
        );
        crate::agent_sessions::append_event(&session_id, stale_projection)
            .expect("persist prior projected turn");

        let mut raw_messages = crate::llm::agent_session_host::visible_messages_with_lineage(
            &session_id,
            vec![serde_json::json!({"role": "user", "content": "resume"})],
        );
        let lineage = crate::llm::message_lineage::take_from_messages(&mut raw_messages)
            .expect("raw visible-message lineage");

        assert_eq!(lineage.projection.policy, "raw");
        assert_eq!(lineage.projection.event_ref, None);
        assert_eq!(lineage.messages[0].source_message_index, Some(0));
    }

    #[test]
    fn visible_messages_preserve_malformed_lineage_as_unknown() {
        let session_id = crate::agent_sessions::open_or_create_for_test(Some(format!(
            "served-context-malformed-lineage-{}",
            uuid::Uuid::now_v7()
        )));
        let mut malformed = serde_json::json!({"role": "user", "content": "continue"});
        malformed[crate::llm::message_lineage::MESSAGE_LINEAGE_KEY] =
            serde_json::json!("malformed");
        let mut messages = crate::llm::agent_session_host::visible_messages_with_lineage(
            &session_id,
            vec![malformed],
        );
        let lineage = crate::llm::message_lineage::take_from_messages(&mut messages)
            .expect("unknown lineage remains structurally available");

        assert_eq!(lineage.projection.policy, "unknown");
        assert_eq!(lineage.messages[0].source_message_index, None);
        assert_eq!(
            lineage.messages[0].semantic_kind,
            crate::llm::message_lineage::MessageSemanticKind::User
        );
        assert!(messages[0]
            .get(crate::llm::message_lineage::MESSAGE_LINEAGE_KEY)
            .is_none());
    }

    #[test]
    fn canonical_directive_is_classified_before_private_metadata_is_stripped() {
        let session_id = crate::agent_sessions::open_or_create_for_test(Some(format!(
            "served-context-directive-{}",
            uuid::Uuid::now_v7()
        )));
        let directive =
            crate::llm::helpers::tracked_directive_envelope_message("directive-1", "instruction");
        let mut messages = crate::llm::agent_session_host::visible_messages_with_lineage(
            &session_id,
            vec![directive],
        );
        let lineage = crate::llm::message_lineage::take_from_messages(&mut messages)
            .expect("directive lineage");
        crate::llm::helpers::strip_directive_commit_metadata(&mut messages);

        assert_eq!(
            lineage.messages[0].semantic_kind,
            crate::llm::message_lineage::MessageSemanticKind::ContextDirective
        );
        assert!(!crate::llm::helpers::has_directive_commit_metadata(
            &messages[0]
        ));

        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.messages = messages;
        opts.message_lineage = Some(lineage);
        let payload = crate::llm::api::LlmRequestPayload::from(&opts);
        let receipt = served_context_receipt(
            &payload,
            &opts.context_manifest,
            &[],
            &opts,
            3,
            "directive-call",
            Some(&session_id),
        );
        assert_eq!(
            receipt["message_lineage"]["messages"][0]["semantic_kind"],
            serde_json::json!("context_directive")
        );
    }

    #[test]
    fn replacement_egress_downgrades_unaligned_summary_lineage_to_unknown() {
        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.system = Some("replacement root".to_string());
        opts.system_prompt_root = crate::llm::prompt::PromptRoot::Replacement;
        opts.messages = vec![
            serde_json::json!({"role": "system", "content": "condensed"}),
            serde_json::json!({"role": "user", "content": "continue"}),
        ];
        opts.message_lineage = Some(crate::llm::message_lineage::MessageLineageManifest {
            projection: crate::llm::message_lineage::ProjectionLineage {
                policy: "summary_prefix".to_string(),
                event_ref: Some("projection-event".to_string()),
                prefix_hash: Some("sha256:projected".to_string()),
            },
            messages: vec![
                crate::llm::message_lineage::MessageLineageEntry {
                    semantic_kind:
                        crate::llm::message_lineage::MessageSemanticKind::CondensedMemory,
                    ..Default::default()
                },
                crate::llm::message_lineage::MessageLineageEntry {
                    semantic_kind: crate::llm::message_lineage::MessageSemanticKind::User,
                    source_message_index: Some(2),
                    ..Default::default()
                },
            ],
        });

        let payload = crate::llm::api::LlmRequestPayload::from(&opts);
        assert_eq!(
            payload.messages.len(),
            1,
            "replacement root removes system history"
        );
        let receipt = served_context_receipt(
            &payload,
            &opts.context_manifest,
            &[],
            &opts,
            4,
            "replacement-call",
            None,
        );
        let manifest = &receipt["message_lineage"];
        assert_eq!(
            manifest["projection"]["policy"],
            serde_json::json!("unknown")
        );
        assert!(manifest["projection"].get("event_ref").is_none());
        assert_eq!(
            manifest["messages"][0]["semantic_kind"],
            serde_json::json!("unknown")
        );
        assert!(manifest["messages"][0]
            .get("source_message_index")
            .is_none());
    }

    #[test]
    fn native_directive_egress_reorder_downgrades_positional_lineage_to_unknown() {
        use crate::llm::message_lineage::{
            MessageLineageEntry, MessageLineageManifest, MessageSemanticKind,
        };

        let mut opts = crate::llm::api::options::base_opts("anthropic");
        opts.model = "claude-opus-4-8".to_string();
        opts.messages = vec![
            serde_json::json!({"role": "user", "content": "work"}),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_01", "name": "read", "input": {}},
                {"type": "server_tool_use", "id": "srvtoolu_01", "name": "search", "input": {}},
            ]}),
            serde_json::json!({"role": "system", "content": "constraint"}),
            serde_json::json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_01", "content": "result"},
            ]}),
            serde_json::json!({"role": "assistant", "content": "continue"}),
        ];
        opts.message_lineage = Some(MessageLineageManifest {
            projection: crate::llm::message_lineage::raw_projection(),
            messages: [
                MessageSemanticKind::User,
                MessageSemanticKind::AssistantToolCall,
                MessageSemanticKind::Instruction,
                MessageSemanticKind::ToolResult,
                MessageSemanticKind::Assistant,
            ]
            .into_iter()
            .enumerate()
            .map(
                |(source_message_index, semantic_kind)| MessageLineageEntry {
                    semantic_kind,
                    source_message_index: Some(source_message_index),
                    ..Default::default()
                },
            )
            .collect(),
        });

        let payload = crate::llm::api::LlmRequestPayload::from(&opts);
        assert_eq!(payload.messages.len(), opts.messages.len());
        assert_eq!(payload.messages[2], opts.messages[3]);
        assert_eq!(payload.messages[3]["role"], serde_json::json!("system"));
        assert!(payload.aligned_message_lineage(&opts).is_none());

        let receipt = served_context_receipt(
            &payload,
            &opts.context_manifest,
            &[],
            &opts,
            5,
            "native-reordered-call",
            None,
        );
        let manifest = &receipt["message_lineage"];
        assert_eq!(
            manifest["projection"]["policy"],
            serde_json::json!("unknown")
        );
        assert!(manifest["messages"]
            .as_array()
            .expect("message lineage")
            .iter()
            .all(|message| message.get("source_message_index").is_none()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn forced_compaction_context_is_exactly_reconstructable_without_a_provider() {
        let stable_goal = "stable_goal";
        let recent_failure = "recent_failure";
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "archived task context"}),
            serde_json::json!({"role": "assistant", "content": "archived investigation"}),
            serde_json::json!({"role": "user", "content": "continue from current evidence"}),
            serde_json::json!({
                "role": "assistant",
                "content": "checking",
                "tool_calls": [{"id": "tool-1", "name": "verify", "arguments": {}}],
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "tool-1",
                "content": recent_failure,
            }),
        ];
        let config = crate::orchestration::AutoCompactConfig {
            token_threshold: 0,
            keep_last: 3,
            ..crate::orchestration::AutoCompactConfig::default()
        };
        let compacted =
            crate::orchestration::auto_compact_messages_with_result(&mut messages, &config, None)
                .await
                .expect("deterministic compaction")
                .expect("forced compaction must fire");
        let condensed_memory = compacted.summary;
        assert_eq!(messages[0]["content"], serde_json::json!(&condensed_memory));

        let session_id = crate::agent_sessions::open_or_create_for_test(Some(format!(
            "served-context-compaction-{}",
            uuid::Uuid::now_v7()
        )));
        crate::agent_sessions::replace_messages_with_summary(
            &session_id,
            &messages,
            Some(&condensed_memory),
        )
        .expect("persist compacted session");
        let receipt = crate::orchestration::CompactionReceipt {
            schema_version: crate::orchestration::COMPACTION_RECEIPT_SCHEMA_VERSION,
            receipt_id: "compaction-lineage-receipt".to_string(),
            session_id: Some(session_id.clone()),
            mode: "auto".to_string(),
            reason: "threshold".to_string(),
            strategy: "observation_mask".to_string(),
            engine_strategy: "observation_mask".to_string(),
            archived_messages: 2,
            estimated_tokens_before: 1,
            estimated_tokens_after: 1,
            ..crate::orchestration::CompactionReceipt::default()
        };
        let compaction_event = crate::llm::helpers::transcript_event_with_id(
            &receipt.receipt_id,
            "compaction",
            "system",
            "internal",
            "",
            Some(serde_json::json!({"receipt": receipt.to_json()})),
        );
        crate::agent_sessions::append_event(&session_id, compaction_event)
            .expect("persist compaction receipt");

        let transcript = crate::agent_sessions::transcript(&session_id)
            .and_then(|value| value.as_dict().cloned())
            .expect("compacted transcript");
        let projection_policy = crate::stdlib::transcript_project::parse_projection_options(
            &crate::value::VmValue::Nil,
        )
        .expect("default projection policy");
        let mut projection = crate::stdlib::transcript_project::project_transcript(
            None,
            &transcript,
            &projection_policy,
        )
        .await
        .expect("deterministic raw projection");
        let projection_event = crate::stdlib::transcript_project::projection_event_value(
            &projection,
            &projection_policy,
        );
        let projection_event_json = crate::llm::helpers::vm_value_to_json(&projection_event);
        crate::agent_sessions::append_event(&session_id, projection_event)
            .expect("persist projection receipt");
        crate::llm::message_lineage::attach_projection(
            &mut projection.messages,
            &projection.source_message_indices,
            &crate::llm::message_lineage::ProjectionLineage {
                policy: projection_policy.kind.as_str().to_string(),
                event_ref: projection_event_json["id"].as_str().map(str::to_string),
                prefix_hash: Some(projection.prefix_hash.clone()),
            },
        );
        let mut provider_messages = crate::llm::agent_session_host::visible_messages_with_lineage(
            &session_id,
            projection.messages,
        );
        let message_lineage =
            crate::llm::message_lineage::take_from_messages(&mut provider_messages)
                .expect("projection and visible-message owners attach complete lineage");
        crate::llm::helpers::strip_directive_commit_metadata(&mut provider_messages);

        let mut opts = crate::llm::api::options::base_opts("openai");
        opts.model = "gpt-test".to_string();
        opts.system = Some(stable_goal.to_string());
        opts.context_manifest = crate::llm::prompt::ContextAssemblyManifest::internal(
            "test:stable-goal",
            "test",
            "agent_loop",
            opts.system.as_deref(),
        );
        opts.messages = provider_messages;
        opts.message_lineage = Some(message_lineage);
        opts.session_id = Some(session_id);

        let _guard = crate::llm::env_guard();
        let previous_verbose = set_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", None);
        let dir = temp_transcript_dir("harn-served-context-compaction-lineage");
        let dir_string = dir.to_string_lossy().to_string();
        super::super::push_llm_transcript_dir(&dir_string);

        let payload = super::super::dump_llm_request(7, "call-after-compaction", "native", &opts)
            .expect("provider-bound request is observable without dispatch");
        super::super::pop_llm_transcript_dir();
        restore_env_for_test("HARN_LLM_TRANSCRIPT_VERBOSE", previous_verbose);

        let events = read_transcript_events(&dir);
        let system = events
            .iter()
            .find(|event| event["type"] == "system_prompt")
            .expect("stable goal definition");
        assert_eq!(system["content"], serde_json::json!(stable_goal));

        let definitions: std::collections::BTreeMap<String, serde_json::Value> = events
            .iter()
            .filter(|event| event["type"] == "served_message")
            .map(|event| {
                (
                    event["content_hash"]
                        .as_str()
                        .expect("message definition hash")
                        .to_string(),
                    event["message"].clone(),
                )
            })
            .collect();
        let request = events
            .iter()
            .find(|event| event["type"] == "provider_call_request")
            .expect("provider request receipt");
        let lineage = &request["served_context"]["message_lineage"];
        assert_eq!(
            lineage["call_id"],
            serde_json::json!("call-after-compaction")
        );
        assert_eq!(lineage["iteration"], serde_json::json!(7));
        assert_eq!(lineage["projection"]["policy"], serde_json::json!("raw"));
        assert_eq!(
            lineage["projection"]["event_ref"],
            projection_event_json["id"]
        );
        assert_eq!(
            lineage["projection"]["prefix_hash"],
            projection_event_json["metadata"]["prefix_hash"]
        );

        let lineage_messages = lineage["messages"].as_array().expect("ordered lineage");
        let reconstruct = |definitions: &std::collections::BTreeMap<String, serde_json::Value>| {
            lineage_messages
                .iter()
                .map(|entry| {
                    entry["definition_ref"]
                        .as_str()
                        .and_then(|definition_ref| definitions.get(definition_ref))
                        .cloned()
                })
                .collect::<Option<Vec<_>>>()
        };
        let reconstructed = reconstruct(&definitions).expect("complete message definitions");
        assert_eq!(reconstructed, payload.messages);
        assert_eq!(
            reconstructed[0]["content"],
            serde_json::json!(&condensed_memory)
        );
        assert_eq!(
            reconstructed.last().expect("recent message")["content"],
            serde_json::json!(recent_failure)
        );
        assert_eq!(
            lineage_messages[0]["semantic_kind"],
            serde_json::json!("condensed_memory")
        );
        assert_eq!(
            lineage_messages[0]["compaction_receipt_ref"],
            serde_json::json!("compaction-lineage-receipt")
        );
        assert_eq!(
            lineage_messages.last().expect("recent lineage")["semantic_kind"],
            serde_json::json!("tool_result")
        );
        let reconstructed_hash =
            stable_redacted_json_hash(&serde_json::Value::Array(reconstructed));
        assert_eq!(
            request["served_context"]["messages_content_hash"],
            serde_json::json!(reconstructed_hash)
        );

        let missing_ref = lineage_messages[0]["definition_ref"]
            .as_str()
            .expect("falsifier ref");
        let mut incomplete = definitions;
        incomplete.remove(missing_ref);
        assert!(
            reconstruct(&incomplete).is_none(),
            "the negative control must make reconstruction fail closed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
