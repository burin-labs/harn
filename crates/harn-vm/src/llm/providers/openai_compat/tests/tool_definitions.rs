//! Tool definitions and `tool_choice` on the request.

use super::fixtures::base_request_payload;
use crate::llm::providers::openai_compat::OpenAiCompatibleProvider;
use serde_json::json;

#[test]
fn cerebras_request_strips_harn_tool_extensions() {
    let mut payload = base_request_payload();
    payload.provider = "cerebras".to_string();
    payload.model = "gpt-oss-120b".to_string();
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "namespace": "ops",
        "defer_loading": true,
        "function": {
            "name": "deploy",
            "description": "Deploy the app",
            "namespace": "ops",
            "x-harn-output-schema": {"type": "object"},
            "parameters": {
                "type": "object",
                "properties": {
                    "env": {"type": "string"}
                },
                "required": ["env"]
            }
        }
    })]);

    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    let tool = &body["tools"][0];
    assert_eq!(tool["type"], "function");
    assert!(tool.get("namespace").is_none());
    assert!(tool.get("defer_loading").is_none());
    assert!(tool["function"].get("namespace").is_none());
    assert!(tool["function"].get("x-harn-output-schema").is_none());
    assert_eq!(
        tool["function"]["parameters"]["properties"]["env"]["type"],
        "string"
    );
    let source_tool = &payload.native_tools.as_ref().expect("source tools")[0];
    assert_eq!(source_tool["namespace"], "ops");
    assert_eq!(
        source_tool["function"]["x-harn-output-schema"]["type"],
        "object"
    );
}

#[test]
fn openai_strict_schemas_are_sanitized_before_request() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-5.4".to_string();
    payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
        schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "answer": {
                    "type": "string",
                    "minLength": 1,
                    "pattern": "^[a-z]+$",
                    "format": "email",
                    "default": "unknown"
                },
                "variant": {
                    "oneOf": [
                        {"type": "string"},
                        {"type": "integer"}
                    ]
                }
            }
        }),
        strict: true,
    };
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "function": {
            "name": "lookup",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "pattern": "^harn",
                        "minLength": 1,
                        "default": "harn"
                    },
                    "mode": {
                        "oneOf": [
                            {"type": "string"},
                            {"type": "integer"}
                        ]
                    }
                }
            }
        }
    })]);

    let body = OpenAiCompatibleProvider::build_request_body(&payload);

    let response_schema = &body["response_format"]["json_schema"]["schema"];
    assert_eq!(response_schema["additionalProperties"], false);
    assert_eq!(response_schema["required"], json!(["answer", "variant"]));
    assert!(response_schema.get("$schema").is_none());
    assert!(response_schema["properties"]["answer"]
        .get("default")
        .is_none());
    assert!(response_schema["properties"]["answer"]
        .get("minLength")
        .is_none());
    assert!(response_schema["properties"]["answer"]
        .get("pattern")
        .is_none());
    assert!(response_schema["properties"]["answer"]
        .get("format")
        .is_none());
    assert!(response_schema["properties"]["variant"]
        .get("oneOf")
        .is_none());
    assert!(response_schema["properties"]["variant"]["description"]
        .as_str()
        .expect("oneOf compatibility note")
        .contains("Original JSON Schema `oneOf` constraint omitted"));

    let tool_schema = &body["tools"][0]["function"]["parameters"];
    assert_eq!(tool_schema["additionalProperties"], false);
    assert_eq!(tool_schema["required"], json!(["mode", "query"]));
    assert!(tool_schema["properties"]["query"].get("default").is_none());
    assert!(tool_schema["properties"]["query"].get("pattern").is_none());
    assert!(tool_schema["properties"]["query"]
        .get("minLength")
        .is_none());
    assert!(tool_schema["properties"]["mode"].get("oneOf").is_none());
}

/// Strict mode lists every property in `required`, so an optional tool
/// argument must also accept `null` or the model has to invent a value for it
/// (`folder: ""` on a read that only meant `file`). Tool dispatch reads that
/// `null` back as absent. A lenient route keeps optional properties optional,
/// and structured output keeps its plain type because its responses are
/// validated against the caller's original schema.
#[test]
fn openai_strict_tool_optional_arguments_accept_null_on_the_wire() {
    let look_parameters = json!({
        "type": "object",
        "properties": {
            "file": {"type": "string", "description": "File to read."},
            "folder": {"type": "string"},
            "intent": {"type": "string", "enum": ["read", "list"]},
            "label": {"type": ["string", "null"]},
            "range": {
                "type": "object",
                "properties": {
                    "start": {"type": "integer"},
                    "end": {"type": "integer"}
                },
                "required": ["start"]
            }
        },
        "required": ["intent"]
    });
    let tool = |strict: bool| {
        json!({
            "type": "function",
            "function": {"name": "look", "strict": strict, "parameters": look_parameters}
        })
    };
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-5.4".to_string();
    payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
        schema: json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}}
        }),
        strict: true,
    };
    payload.native_tools = Some(vec![tool(true)]);

    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    let parameters = &body["tools"][0]["function"]["parameters"];
    assert_eq!(
        parameters["required"],
        json!(["file", "folder", "intent", "label", "range"])
    );
    let properties = &parameters["properties"];
    assert_eq!(properties["file"]["type"], json!(["string", "null"]));
    assert_eq!(properties["folder"]["type"], json!(["string", "null"]));
    assert!(properties["file"]["description"]
        .as_str()
        .expect("unused-optional note")
        .starts_with("File to read.\nOptional in Harn: send null when not used."));
    assert_eq!(
        properties["intent"]["type"], "string",
        "a required argument stays non-null"
    );
    assert_eq!(
        properties["label"]["type"],
        json!(["string", "null"]),
        "an already-nullable type is not widened twice"
    );
    let range = &properties["range"]["anyOf"];
    assert_eq!(range[1], json!({"type": "null"}));
    assert_eq!(range[0]["additionalProperties"], false);
    assert_eq!(range[0]["required"], json!(["end", "start"]));
    assert_eq!(range[0]["properties"]["start"]["type"], "integer");
    assert_eq!(
        range[0]["properties"]["end"]["type"],
        json!(["integer", "null"]),
        "a nested optional field accepts null too"
    );

    let response_schema = &body["response_format"]["json_schema"]["schema"];
    assert_eq!(response_schema["required"], json!(["answer"]));
    assert_eq!(response_schema["properties"]["answer"]["type"], "string");

    payload.native_tools = Some(vec![tool(false)]);
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    let parameters = &body["tools"][0]["function"]["parameters"];
    assert_eq!(parameters["required"], json!(["intent"]));
    assert_eq!(parameters["properties"]["file"]["type"], "string");
}

#[test]
fn openai_tool_search_request_keeps_wire_extensions() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-5.4".to_string();
    payload.native_tools = Some(vec![
        json!({"type": "tool_search"}),
        json!({
            "type": "function",
            "namespace": "ops",
            "defer_loading": true,
            "function": {
                "name": "deploy",
                "description": "Deploy the app",
                "x-harn-output-schema": {"type": "object"},
                "parameters": {"type": "object"}
            }
        }),
    ]);

    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert_eq!(body["tools"][0], json!({"type": "tool_search"}));
    assert!(body["tools"][1].get("namespace").is_none());
    assert_eq!(body["tools"][1]["defer_loading"], true);
    assert!(
        body["tools"][1]["function"]
            .get("x-harn-output-schema")
            .is_none(),
        "Harn output schemas stay in transcripts, not provider payloads"
    );
}

#[test]
fn openai_regular_request_strips_tool_search_extensions_without_meta_tool() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-5.4".to_string();
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "namespace": "ops",
        "defer_loading": true,
        "function": {
            "name": "deploy",
            "description": "Deploy the app",
            "parameters": {"type": "object"}
        }
    })]);

    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert!(body["tools"][0].get("namespace").is_none());
    assert!(body["tools"][0].get("defer_loading").is_none());
}

#[test]
fn openrouter_kimi27_code_normalizes_forced_tool_choice_to_auto() {
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "moonshotai/kimi-k2.7-code".to_string();
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "function": {
            "name": "add_two",
            "description": "Add two integers.",
            "parameters": {
                "type": "object",
                "properties": {
                    "a": {"type": "integer"},
                    "b": {"type": "integer"}
                },
                "required": ["a", "b"]
            }
        }
    })]);
    payload.tool_choice = Some(json!("required"));

    let body = OpenAiCompatibleProvider::build_request_body(&payload);

    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["tools"][0]["function"]["name"], "add_two");
}

#[test]
fn openrouter_kimi27_code_keeps_allowed_tool_choice_none() {
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "moonshotai/kimi-k2.7-code".to_string();
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "function": {
            "name": "add_two",
            "description": "Add two integers.",
            "parameters": {
                "type": "object",
                "properties": {
                    "a": {"type": "integer"},
                    "b": {"type": "integer"}
                },
                "required": ["a", "b"]
            }
        }
    })]);
    payload.tool_choice = Some(json!("none"));

    let body = OpenAiCompatibleProvider::build_request_body(&payload);

    assert_eq!(body["tool_choice"], "none");
    assert_eq!(body["tools"][0]["function"]["name"], "add_two");
}

#[test]
fn openai_compat_bare_tool_choice_string_becomes_function_selection() {
    let mut payload = base_request_payload();
    payload.provider = "fireworks".to_string();
    payload.model = "accounts/fireworks/models/deepseek-v4-pro".to_string();
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "function": {
            "name": "edit",
            "description": "Edit a file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }
        }
    })]);
    payload.tool_choice = Some(json!("edit"));

    let body = OpenAiCompatibleProvider::build_request_body(&payload);

    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "function": {"name": "edit"}})
    );
    assert_eq!(body["tools"][0]["function"]["name"], "edit");
}

#[test]
fn openai_compat_omits_tool_choice_for_text_tool_routes() {
    let mut payload = base_request_payload();
    payload.provider = "fireworks".to_string();
    payload.model = "accounts/fireworks/models/gpt-oss-120b".to_string();
    payload.tool_choice = Some(json!("edit"));

    let body = OpenAiCompatibleProvider::build_request_body(&payload);

    assert!(body.get("tool_choice").is_none());
    assert!(body.get("tools").is_none());
}

#[test]
fn openai_compat_omits_required_tool_choice_without_native_tools() {
    let mut payload = base_request_payload();
    payload.provider = "together".to_string();
    payload.model = "zai-org/glm-5.2".to_string();
    payload.tool_choice = Some(json!("required"));

    let body = OpenAiCompatibleProvider::build_request_body(&payload);

    assert!(body.get("tool_choice").is_none());
    assert!(body.get("tools").is_none());
}
