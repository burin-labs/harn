//! Wire tests for the Gemini Interactions endpoint family.
//!
//! The request shapes asserted here were captured from live
//! `POST /v1beta/interactions` calls, including the negative ones: a stateless
//! tool replay that omits the `thought` step is rejected by the provider with
//! `invalid_request`, which is why the signature assertions are positional
//! rather than "somewhere in the body".

use super::interactions::{parse_response, thinking_level, GeminiInteractions};
use super::interactions_stream::{InteractionStream, StreamAction};
use super::test_support::gemini_payload;
use crate::llm::api::{OutputFormat, ReasoningEffort, ThinkingConfig};
use crate::llm::capabilities::{
    clear_user_overrides, lookup, lookup_with_user_overrides, CapabilitiesFile, LiveEndpointFamily,
};
use serde_json::{json, Value};

const MODEL: &str = "gemini-2.5-flash";

fn interactions_dialect() -> crate::llm::api::DialectContract {
    crate::llm::api::DialectContract::new(
        crate::llm::capabilities::WireDialect::Gemini,
        Some(LiveEndpointFamily::GeminiInteractions),
    )
}

fn interactions_override() -> CapabilitiesFile {
    toml::from_str(
        // `extends` is what makes this a one-field overlay: without it the
        // row would REPLACE the shipped gemini-2.5-flash row and take the
        // thinking budgets, vision, and file-API flags down with it.
        "[[provider.gemini]]\n\
         model_match = \"gemini-2.5-flash*\"\n\
         extends = true\n\
         live_endpoint_family = \"gemini_interactions\"\n",
    )
    .expect("override parses")
}

// ---------------------------------------------------------------------------
// Capability resolution
// ---------------------------------------------------------------------------

#[test]
fn gemini_routes_default_to_generate_content() {
    clear_user_overrides();
    let caps = lookup("gemini", MODEL);
    assert_eq!(
        caps.live_endpoint_family,
        Some(LiveEndpointFamily::GeminiGenerateContent),
        "an unset row on the Gemini dialect must keep the legacy live endpoint"
    );
}

#[test]
fn current_gemini_models_route_to_interactions_and_strip_sampling_controls() {
    clear_user_overrides();
    // 3.8 Flash is listed first so the loop proves the claim that matters for
    // it: Google's endpoint ACCEPTS temperature / top_p / top_k with a 200,
    // and the only thing keeping them off the wire is this strip. Both the
    // bare id and the `models/` REST resource name must route the same way.
    for model in [
        "gemini-3.8-flash",
        "models/gemini-3.8-flash",
        "gemini-3.6-flash",
        "gemini-3.5-flash-lite",
    ] {
        let caps = lookup("gemini", model);
        assert_eq!(
            caps.live_endpoint_family,
            Some(LiveEndpointFamily::GeminiInteractions),
            "{model} endpoint family"
        );
        assert!(!caps.temperature_supported, "{model} temperature");
        assert!(!caps.top_p_supported, "{model} top_p");
        assert!(!caps.top_k_supported, "{model} top_k");

        let mut payload = gemini_payload(
            model,
            ThinkingConfig::Effort {
                level: ReasoningEffort::Medium,
            },
        );
        payload.temperature = Some(0.2);
        payload.top_p = Some(0.8);
        payload.top_k = Some(20);
        let body =
            crate::llm::api::DialectContract::for_request(&payload).build_request_body(&payload);
        assert_eq!(body["generation_config"]["thinking_level"], "medium");
        assert!(body["generation_config"].get("temperature").is_none());
        assert!(body["generation_config"].get("top_p").is_none());
        assert!(body["generation_config"].get("top_k").is_none());
        assert!(body.get("contents").is_none());
        assert!(body.get("tools").is_none(), "fixture has no tools");
    }
}

#[test]
fn option_probe_preserves_only_the_selected_interactions_sampling_field() {
    use crate::llm::capabilities::PortableOption;

    let mut payload = gemini_payload(
        "gemini-3.6-flash",
        ThinkingConfig::Effort {
            level: ReasoningEffort::Medium,
        },
    );
    payload.temperature = Some(0.2);
    payload.top_p = Some(0.8);
    payload.top_k = Some(20);
    payload.provider_contract_probe = Some(PortableOption::TopK);
    let body = crate::llm::api::DialectContract::for_request(&payload).build_request_body(&payload);

    assert!(body["generation_config"].get("temperature").is_none());
    assert!(body["generation_config"].get("top_p").is_none());
    assert_eq!(body["generation_config"]["top_k"], json!(20));
}

#[test]
fn disabled_thinking_uses_the_models_lowest_supported_interactions_level() {
    let caps = crate::llm::capabilities::Capabilities {
        reasoning_effort_levels: vec!["low".into(), "medium".into(), "high".into()],
        ..Default::default()
    };

    assert_eq!(
        thinking_level(&ThinkingConfig::Disabled, &caps).as_deref(),
        Some("low")
    );
}

#[test]
fn non_gemini_routes_have_no_endpoint_family() {
    clear_user_overrides();
    assert_eq!(
        lookup("anthropic", "claude-opus-4-5").live_endpoint_family,
        None,
        "dialects with a single live endpoint must not advertise a family"
    );
}

#[test]
fn project_override_selects_interactions_without_moving_batch() {
    clear_user_overrides();
    let caps = lookup_with_user_overrides("gemini", MODEL, Some(&interactions_override()));
    assert_eq!(
        caps.live_endpoint_family,
        Some(LiveEndpointFamily::GeminiInteractions)
    );
    assert!(caps.live_endpoint_family.unwrap().is_stateful());
    // The whole point of a second axis: Gemini Batch only accepts
    // `generateContent` bodies, so selecting Interactions for live traffic must
    // leave the batch family exactly where it was.
    assert_eq!(
        caps.batch_wire_format.as_deref(),
        Some("gemini"),
        "selecting the Interactions live family must not move Gemini Batch"
    );
    assert_eq!(
        caps.message_wire_format,
        crate::llm::capabilities::WireDialect::Gemini,
        "the endpoint family is a second axis, not a replacement dialect"
    );
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

#[test]
fn text_turn_builds_typed_user_input_steps() {
    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.system = Some("be terse".to_string());
    payload.temperature = Some(0.25);
    payload.top_p = Some(0.9);
    payload.top_k = Some(20);
    payload.stop = Some(vec!["STOP".to_string()]);
    let body = GeminiInteractions::build_request_body(&payload);

    assert_eq!(body["model"], MODEL);
    assert_eq!(body["input"][0]["type"], "user_input");
    assert_eq!(body["input"][0]["content"][0]["type"], "text");
    assert_eq!(body["input"][0]["content"][0]["text"], "hello");
    assert_eq!(body["system_instruction"], "be terse");
    assert_eq!(body["generation_config"]["max_output_tokens"], 64);
    assert_eq!(body["generation_config"]["temperature"], 0.25);
    assert_eq!(body["generation_config"]["top_p"], 0.9);
    assert_eq!(body["generation_config"]["top_k"], 20);
    assert_eq!(body["generation_config"]["stop_sequences"][0], "STOP");
    // `generateContent` spellings must never leak onto this family.
    assert!(body.get("contents").is_none());
    assert!(body.get("generationConfig").is_none());
}

#[test]
fn state_is_not_stored_unless_the_caller_asked_for_it() {
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    assert_eq!(
        GeminiInteractions::build_request_body(&payload)["store"],
        json!(false),
        "Harn owns transcripts; a plain turn must not be retained provider-side"
    );

    let mut chained = gemini_payload(MODEL, ThinkingConfig::Disabled);
    chained.previous_response_id = Some("v1_abc".to_string());
    let body = GeminiInteractions::build_request_body(&chained);
    assert_eq!(body["previous_interaction_id"], "v1_abc");
    assert_eq!(
        body["store"],
        json!(true),
        "chaining is only resolvable while the interaction is stored"
    );

    let mut explicit = gemini_payload(MODEL, ThinkingConfig::Disabled);
    explicit.previous_response_id = Some("v1_abc".to_string());
    explicit.store = Some(false);
    assert_eq!(
        GeminiInteractions::build_request_body(&explicit)["store"],
        json!(false),
        "an explicit store wins over the chaining default"
    );
}

#[test]
fn tools_are_flattened_and_tool_choice_preserves_constraints() {
    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.native_tools = Some(vec![
        json!({
            "name": "set_light",
            "description": "Sets brightness.",
            "input_schema": {
                "type": "object",
                "properties": {"brightness": {"type": "integer"}},
                "required": ["brightness"],
            },
        }),
        json!({
            "name": "read_light",
            "description": "Reads brightness.",
            "input_schema": {"type": "object"},
        }),
    ]);
    payload.tool_choice = Some(json!({"type": "function", "function": {"name": "set_light"}}));
    let body = GeminiInteractions::build_request_body(&payload);

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "set_light");
    assert_eq!(
        body["tools"][0]["parameters"]["properties"]["brightness"]["type"],
        "integer"
    );
    assert!(
        body["tools"][0].get("functionDeclarations").is_none(),
        "Interactions takes flat function tools, not the generateContent envelope"
    );
    assert_eq!(body["tools"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        body["generation_config"]["tool_choice"],
        json!({"allowed_tools": {"mode": "any", "tools": ["set_light"]}})
    );

    let mut bare_name = gemini_payload(MODEL, ThinkingConfig::Disabled);
    bare_name.native_tools = payload.native_tools;
    bare_name.tool_choice = Some(json!("set_light"));
    assert_eq!(
        GeminiInteractions::build_request_body(&bare_name)["generation_config"]["tool_choice"],
        json!({"allowed_tools": {"mode": "any", "tools": ["set_light"]}})
    );

    for (choice, expected) in [
        (json!("none"), "none"),
        (json!("required"), "any"),
        (json!("any"), "any"),
        (json!("auto"), "auto"),
    ] {
        let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
        payload.tool_choice = Some(choice);
        assert_eq!(
            GeminiInteractions::build_request_body(&payload)["generation_config"]["tool_choice"],
            expected
        );
    }
}

#[test]
fn thinking_maps_onto_the_level_ladder_and_requests_summaries() {
    for (thinking, level, summaries) in [
        (ThinkingConfig::Disabled, Some("minimal"), false),
        (
            ThinkingConfig::Effort {
                level: ReasoningEffort::Low,
            },
            Some("low"),
            true,
        ),
        (
            ThinkingConfig::Effort {
                level: ReasoningEffort::Medium,
            },
            Some("medium"),
            true,
        ),
        (
            ThinkingConfig::Effort {
                level: ReasoningEffort::XHigh,
            },
            Some("high"),
            true,
        ),
        // A token budget has no rung on a four-level ladder, so it is dropped
        // rather than guessed at.
        (
            ThinkingConfig::Enabled {
                budget_tokens: Some(4096),
            },
            None,
            true,
        ),
        (ThinkingConfig::Adaptive, None, true),
    ] {
        let payload = gemini_payload(MODEL, thinking.clone());
        let config = &GeminiInteractions::build_request_body(&payload)["generation_config"];
        match level {
            Some(level) => assert_eq!(config["thinking_level"], level, "for {thinking:?}"),
            None => assert!(
                config.get("thinking_level").is_none(),
                "a token budget must not be forced onto a level for {thinking:?}"
            ),
        }
        assert_eq!(
            config.get("thinking_summaries").is_some(),
            summaries,
            "summaries are requested exactly when thinking is enabled, for {thinking:?}"
        );
    }
}

#[test]
fn structured_output_uses_response_format() {
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    assert!(
        GeminiInteractions::build_request_body(&payload)
            .get("response_format")
            .is_none(),
        "plain text must not acquire a structured-output constraint"
    );

    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.output_format = OutputFormat::JsonObject;
    assert_eq!(
        GeminiInteractions::build_request_body(&payload)["response_format"],
        json!({
            "type": "text",
            "mime_type": "application/json",
            "schema": {"type": "object"},
        })
    );

    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.output_format = OutputFormat::JsonSchema {
        schema: json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {"city": {"type": "string", "default": "London"}},
            "required": ["city"],
        }),
        strict: true,
    };
    let body = GeminiInteractions::build_request_body(&payload);
    let response_format = &body["response_format"];
    assert_eq!(response_format["type"], "text");
    assert_eq!(response_format["mime_type"], "application/json");
    let sanitized = &response_format["schema"];
    assert_eq!(sanitized["type"], "object");
    assert_eq!(sanitized["required"], json!(["city"]));
    assert_eq!(sanitized["properties"]["city"]["type"], "string");
    assert!(sanitized.get("additionalProperties").is_none());
    assert!(sanitized["properties"]["city"].get("default").is_none());
    assert!(
        response_format.get("properties").is_none(),
        "the retired bare-schema shape must not survive beside the current contract"
    );
}

#[test]
fn media_content_is_restated_in_the_typed_block_vocabulary() {
    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.vision = true;
    payload.messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "what is this?"},
            {"type": "image", "base64": "AAAA", "media_type": "image/png"},
            {"type": "image", "url": "https://example.com/i.png", "media_type": "image/png"},
        ],
    })];
    let content = &GeminiInteractions::build_request_body(&payload)["input"][0]["content"];
    assert_eq!(content[0], json!({"type": "text", "text": "what is this?"}));
    assert_eq!(
        content[1],
        json!({"type": "image", "data": "AAAA", "mime_type": "image/png"})
    );
    assert_eq!(
        content[2],
        json!({"type": "image", "uri": "https://example.com/i.png", "mime_type": "image/png"})
    );
}

/// A stateless tool loop: the opaque thought signature has to precede the call
/// it authorizes. Omitting it is not a degraded request — the provider rejects
/// the whole turn.
#[test]
fn stateless_tool_replay_emits_thought_then_call_then_result() {
    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.messages = vec![
        json!({"role": "user", "content": "dim the lights"}),
        json!({
            "role": "assistant",
            "content": [{
                "type": "tool_call",
                "id": "call_abc",
                "name": "set_light",
                "arguments": {"brightness": 20},
                "thought_signature": "sig-xyz",
            }],
        }),
        json!({
            "role": "tool",
            "name": "set_light",
            "tool_call_id": "call_abc",
            "content": "{\"ok\":true}",
        }),
    ];
    let input = GeminiInteractions::build_request_body(&payload)["input"].clone();

    assert_eq!(input[0]["type"], "user_input");
    assert_eq!(input[1], json!({"type": "thought", "signature": "sig-xyz"}));
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["id"], "call_abc");
    assert_eq!(input[2]["name"], "set_light");
    assert_eq!(input[2]["arguments"], json!({"brightness": 20}));
    assert_eq!(input[3]["type"], "function_result");
    assert_eq!(input[3]["call_id"], "call_abc");
    assert_eq!(input[3]["name"], "set_light");
    assert_eq!(input[3]["result"][0]["type"], "text");
    assert_eq!(input.as_array().expect("input is a list").len(), 4);
}

/// Regression for the defect that took down every direct-route tool loop.
///
/// Google returns an empty text part alongside a tool-calls-only model turn.
/// Interactions then refuses a text content block that carries no text, with
/// `invalid_request: Missing text in content of type text.`, and the refusal
/// kills the whole request rather than degrading. Echoing the model turn back
/// verbatim therefore sends exactly the block the endpoint will not accept.
///
/// Captured on the wire before the fix, as the second step of the second
/// request: `{"type": "model_output", "content": [{"type": "text", "text": ""}]}`.
#[test]
fn an_empty_model_text_part_never_becomes_an_empty_text_block() {
    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.messages = vec![
        json!({"role": "user", "content": "read the spec"}),
        json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": ""},
                {
                    "type": "tool_call",
                    "id": "call_abc",
                    "name": "read_file",
                    "arguments": {"path": "spec.md"},
                    "thought_signature": "sig-xyz",
                },
            ],
        }),
        json!({
            "role": "tool",
            "name": "read_file",
            "tool_call_id": "call_abc",
            "content": "the greeting must say hello",
        }),
    ];
    let input = GeminiInteractions::build_request_body(&payload)["input"].clone();
    let steps = input.as_array().expect("input is a list").clone();

    assert!(
        !steps.iter().any(|step| step["type"] == "model_output"),
        "an assistant turn whose only text is empty must not emit a model_output step: {input}"
    );
    assert!(
        !steps.iter().any(empty_text_block_anywhere),
        "no step may carry a text block with no text: {input}"
    );
    // The turn is still replayed: the signature and the call have to survive,
    // or the provider refuses the request for a different reason.
    assert_eq!(steps[1], json!({"type": "thought", "signature": "sig-xyz"}));
    assert_eq!(steps[2]["type"], "function_call");
    assert_eq!(steps[3]["type"], "function_result");

    // Direction control. Real assistant text must still be sent, so the fix
    // cannot pass by dropping model output in general.
    payload.messages[1]["content"][0]["text"] = json!("reading it now");
    let with_text = GeminiInteractions::build_request_body(&payload)["input"].clone();
    let model_output = with_text
        .as_array()
        .expect("input is a list")
        .iter()
        .find(|step| step["type"] == "model_output")
        .expect("a turn with real text still emits a model_output step")
        .clone();
    assert_eq!(model_output["content"][0]["text"], "reading it now");
}

/// The other side of the turn cannot violate the same rule, and this pins why.
///
/// A handler that returns nothing does not reach the wire as an empty text
/// block, because the shared payload normalizer always hands back an object:
/// an empty string becomes `{"result": ""}`, which serializes to text. There is
/// no guard in the result path on purpose. This test is what makes that safe to
/// rely on, so a later change to the normalizer that lets a bare empty string
/// through fails here rather than in production against the provider.
#[test]
fn a_tool_result_with_no_text_still_carries_text() {
    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.messages = vec![
        json!({"role": "user", "content": "dim the lights"}),
        json!({
            "role": "assistant",
            "content": [{
                "type": "tool_call",
                "id": "call_abc",
                "name": "set_light",
                "arguments": {"brightness": 20},
                "thought_signature": "sig-xyz",
            }],
        }),
        json!({
            "role": "tool",
            "name": "set_light",
            "tool_call_id": "call_abc",
            "content": "",
        }),
    ];
    let input = GeminiInteractions::build_request_body(&payload)["input"].clone();
    let steps = input.as_array().expect("input is a list").clone();

    assert!(
        !steps.iter().any(empty_text_block_anywhere),
        "a silent handler must not reach the wire as a text block with no text: {input}"
    );
    let result = steps
        .iter()
        .find(|step| step["type"] == "function_result")
        .expect("the result step is still sent")
        .clone();
    assert_eq!(result["call_id"], "call_abc");
    assert_eq!(result["result"][0]["type"], "text");
    assert_eq!(
        result["result"][0]["text"], "{\"result\":\"\"}",
        "the normalizer, not a guard in this file, is what keeps this non-empty"
    );

    // Direction control: a handler that did return text still sends it.
    payload.messages[2]["content"] = json!("brightness set to 20");
    let with_text = GeminiInteractions::build_request_body(&payload)["input"].clone();
    let answered = with_text
        .as_array()
        .expect("input is a list")
        .iter()
        .find(|step| step["type"] == "function_result")
        .expect("the result step is still sent")
        .clone();
    assert_eq!(
        answered["result"][0]["text"],
        "{\"result\":\"brightness set to 20\"}"
    );
}

/// True when any content or result list on this step holds a text block whose
/// text is empty, which is the exact shape Interactions refuses.
fn empty_text_block_anywhere(step: &Value) -> bool {
    ["content", "result"].iter().any(|field| {
        step[field].as_array().is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block["type"] == "text" && block["text"] == "")
        })
    })
}

/// The stateful variant: the provider already holds every step through the last
/// assistant turn, so replaying them would double both the history and the bill.
#[test]
fn chained_tool_replay_sends_only_the_new_result() {
    let mut payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    payload.previous_response_id = Some("v1_prev".to_string());
    payload.messages = vec![
        json!({"role": "user", "content": "dim the lights"}),
        json!({
            "role": "assistant",
            "content": [{
                "type": "tool_call",
                "id": "call_abc",
                "name": "set_light",
                "arguments": {"brightness": 20},
                "thought_signature": "sig-xyz",
            }],
        }),
        json!({
            "role": "tool",
            "name": "set_light",
            "tool_call_id": "call_abc",
            "content": "{\"ok\":true}",
        }),
    ];
    let body = GeminiInteractions::build_request_body(&payload);
    assert_eq!(body["previous_interaction_id"], "v1_prev");
    let input = body["input"].as_array().expect("input is a list");
    assert_eq!(
        input.len(),
        1,
        "chained turns must not replay server-held history: {input:?}"
    );
    assert_eq!(input[0]["type"], "function_result");
    assert_eq!(input[0]["call_id"], "call_abc");
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

#[test]
fn parses_tool_call_steps_with_the_authorizing_signature() {
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let response = json!({
        "id": "v1_abc",
        "object": "interaction",
        "status": "requires_action",
        "model": MODEL,
        "steps": [
            {"type": "thought", "signature": "sig-xyz"},
            {
                "type": "function_call",
                "id": "LbqAXqM9",
                "name": "set_light_values",
                "arguments": {"brightness": 20, "color_temp": "warm"},
            },
        ],
        "usage": {
            "total_input_tokens": 45,
            "total_output_tokens": 32,
            "total_thought_tokens": 8,
            "total_cached_tokens": 3,
        },
    });
    let result = parse_response(&response, &payload).expect("parses");

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["id"], "LbqAXqM9");
    assert_eq!(result.tool_calls[0]["name"], "set_light_values");
    assert_eq!(result.tool_calls[0]["arguments"]["brightness"], 20);
    assert_eq!(
        result.tool_calls[0]["thought_signature"], "sig-xyz",
        "the thought step authorizes the calls that follow it"
    );
    assert_eq!(result.raw_tool_calls.len(), 1);
    assert_eq!(result.stop_reason.as_deref(), Some("requires_action"));
    assert_eq!(result.input_tokens, 45);
    assert_eq!(result.output_tokens, 40, "thought tokens bill as output");
    assert_eq!(result.cache_read_tokens, 3);
    assert_eq!(result.telemetry.request_id.as_deref(), Some("v1_abc"));

    let tool_block = result
        .blocks
        .iter()
        .find(|block| block["type"] == "tool_call")
        .expect("tool call block");
    assert_eq!(tool_block["thought_signature"], "sig-xyz");
    assert_eq!(tool_block["visibility"], "internal");
}

#[test]
fn parses_model_output_and_thought_summary_into_split_channels() {
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let response = json!({
        "id": "v1_abc",
        "status": "completed",
        "steps": [
            {
                "type": "thought",
                "signature": "sig-xyz",
                "summary": [{"type": "text", "text": "Multiplying 12 by 12."}],
            },
            {"type": "model_output", "content": [{"type": "text", "text": "12 * 12 = 144"}]},
        ],
        "usage": {"total_input_tokens": 11, "total_output_tokens": 13},
    });
    let result = parse_response(&response, &payload).expect("parses");

    assert_eq!(result.text, "12 * 12 = 144");
    assert_eq!(result.thinking.as_deref(), Some("Multiplying 12 by 12."));
    assert_eq!(result.stop_reason.as_deref(), Some("completed"));
    let reasoning = result
        .blocks
        .iter()
        .find(|block| block["type"] == "reasoning")
        .expect("reasoning block");
    assert_eq!(
        reasoning["visibility"], "private",
        "chain-of-thought must never reach the user-facing transcript"
    );
    let text = result
        .blocks
        .iter()
        .find(|block| block["type"] == "output_text")
        .expect("output block");
    assert_eq!(
        text["provider_metadata"]["gemini"]["thought_signature"], "sig-xyz",
        "the signature has to survive into the transcript to be replayable"
    );
}

#[test]
fn surfaces_the_interactions_error_envelope() {
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let response = json!({
        "error": {"message": "Request contains an invalid argument.", "code": "invalid_request"},
    });
    let error = parse_response(&response, &payload).expect_err("errors");
    assert!(
        format!("{error:?}").contains("Request contains an invalid argument."),
        "{error:?}"
    );
}

/// Interactions signals an exhausted output-token budget only as the
/// `incomplete` lifecycle status. Left verbatim it canonicalizes to `end_turn`
/// and Harn's truncation handling never fires.
#[test]
fn budget_truncation_normalizes_into_the_length_vocabulary() {
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let truncated = json!({
        "status": "incomplete",
        "steps": [{"type": "model_output", "content": [{"type": "text", "text": "Rivers are"}]}],
        "usage": {"total_input_tokens": 12, "total_output_tokens": 36},
    });
    let result = parse_response(&truncated, &payload).expect("parses");
    assert_eq!(result.stop_reason.as_deref(), Some("max_tokens"));
    assert!(crate::llm::api::result::stop_reason_is_length(
        result.stop_reason.as_deref().expect("stop reason")
    ));

    // Every other lifecycle status is provider truth and passes through.
    for status in ["completed", "requires_action", "budget_exceeded", "failed"] {
        let response = json!({"status": status, "steps": []});
        let result = parse_response(&response, &payload).expect("parses");
        assert_eq!(result.stop_reason.as_deref(), Some(status));
        assert!(!crate::llm::api::result::stop_reason_is_length(status));
    }
}

#[test]
fn unknown_step_types_are_ignored_rather_than_failing_the_turn() {
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let response = json!({
        "status": "completed",
        "steps": [
            {"type": "some_future_step", "payload": {"anything": true}},
            {"type": "model_output", "content": [{"type": "text", "text": "ok"}]},
        ],
    });
    let result = parse_response(&response, &payload).expect("parses");
    assert_eq!(result.text, "ok");
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

fn drive(events: &[Value]) -> (InteractionStream, Vec<String>) {
    let mut stream = InteractionStream::new();
    let mut deltas = Vec::new();
    for event in events {
        match stream.push(event) {
            StreamAction::Text(text) => deltas.push(text),
            StreamAction::Done | StreamAction::None => {}
        }
    }
    (stream, deltas)
}

#[test]
fn streamed_text_reassembles_into_the_same_envelope() {
    let (stream, deltas) = drive(&[
        json!({"event_type": "interaction.created", "interaction": {"id": "", "status": "in_progress"}}),
        json!({"event_type": "step.start", "index": 0, "step": {"type": "model_output"}}),
        json!({"event_type": "step.delta", "index": 0, "delta": {"type": "text", "text": "1,"}}),
        json!({"event_type": "step.delta", "index": 0, "delta": {"type": "text", "text": " 2, 3"}}),
        json!({"event_type": "step.stop", "index": 0}),
        json!({
            "event_type": "interaction.completed",
            "interaction": {
                "id": "v1_abc",
                "status": "completed",
                "usage": {"total_input_tokens": 11, "total_output_tokens": 13},
            },
        }),
    ]);
    assert_eq!(deltas, vec!["1,".to_string(), " 2, 3".to_string()]);

    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let result = parse_response(&stream.finish(), &payload).expect("parses");
    assert_eq!(result.text, "1, 2, 3");
    assert_eq!(result.stop_reason.as_deref(), Some("completed"));
    assert_eq!(result.input_tokens, 11);
    assert_eq!(
        result.telemetry.request_id.as_deref(),
        Some("v1_abc"),
        "the empty id on interaction.created must not overwrite the real one"
    );
}

/// Tool arguments arrive as a JSON object split at arbitrary byte boundaries;
/// only the concatenation is parseable.
#[test]
fn streamed_partial_tool_arguments_concatenate_before_parsing() {
    let (stream, deltas) = drive(&[
        json!({"event_type": "step.start", "index": 0, "step": {"type": "thought"}}),
        json!({"event_type": "step.delta", "index": 0, "delta": {"type": "thought_signature", "signature": "sig-"}}),
        json!({"event_type": "step.delta", "index": 0, "delta": {"type": "thought_signature", "signature": "xyz"}}),
        json!({"event_type": "step.stop", "index": 0}),
        json!({
            "event_type": "step.start",
            "index": 1,
            "step": {"id": "6L4qZBU4", "type": "function_call", "name": "get_weather", "arguments": {}},
        }),
        json!({"event_type": "step.delta", "index": 1, "delta": {"type": "arguments_delta", "arguments": "{\"city\": \"Pa"}}),
        json!({"event_type": "step.delta", "index": 1, "delta": {"type": "arguments_delta", "arguments": "ris\"}"}}),
        json!({"event_type": "step.stop", "index": 1}),
        json!({"event_type": "interaction.completed", "interaction": {"id": "v1_abc", "status": "requires_action"}}),
    ]);
    assert!(
        deltas.is_empty(),
        "tool-call arguments are not user-visible assistant text"
    );

    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let result = parse_response(&stream.finish(), &payload).expect("parses");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["id"], "6L4qZBU4");
    assert_eq!(result.tool_calls[0]["name"], "get_weather");
    assert_eq!(result.tool_calls[0]["arguments"], json!({"city": "Paris"}));
    assert_eq!(result.tool_calls[0]["thought_signature"], "sig-xyz");
    assert_eq!(result.stop_reason.as_deref(), Some("requires_action"));
}

#[test]
fn parallel_tool_calls_keep_their_wire_order() {
    let (stream, _) = drive(&[
        json!({"event_type": "step.start", "index": 0, "step": {"id": "a", "type": "function_call", "name": "f"}}),
        json!({"event_type": "step.start", "index": 1, "step": {"id": "b", "type": "function_call", "name": "f"}}),
        json!({"event_type": "step.delta", "index": 1, "delta": {"arguments": "{\"n\":2}"}}),
        json!({"event_type": "step.delta", "index": 0, "delta": {"arguments": "{\"n\":1}"}}),
        json!({"event_type": "step.stop", "index": 1}),
        json!({"event_type": "step.stop", "index": 0}),
    ]);
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let result = parse_response(&stream.finish(), &payload).expect("parses");
    assert_eq!(result.tool_calls.len(), 2);
    assert_eq!(result.tool_calls[0]["id"], "a");
    assert_eq!(result.tool_calls[0]["arguments"], json!({"n": 1}));
    assert_eq!(result.tool_calls[1]["id"], "b");
    assert_eq!(result.tool_calls[1]["arguments"], json!({"n": 2}));
}

#[test]
fn a_stream_truncated_mid_arguments_drops_the_partial_call_shape() {
    let (stream, _) = drive(&[
        json!({"event_type": "step.start", "index": 0, "step": {"id": "a", "type": "function_call", "name": "f", "arguments": {}}}),
        json!({"event_type": "step.delta", "index": 0, "delta": {"arguments": "{\"city\": \"Pa"}}),
    ]);
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let result = parse_response(&stream.finish(), &payload).expect("parses");
    assert_eq!(
        result.tool_calls[0]["arguments"],
        json!({}),
        "half-arrived arguments must not be handed to a tool as if complete"
    );
}

#[test]
fn a_streamed_error_event_surfaces_as_a_thrown_error() {
    let (stream, _) = drive(&[
        json!({"event_type": "step.start", "index": 0, "step": {"type": "model_output"}}),
        json!({"event_type": "error", "error": {"code": "invalid_request", "message": "boom"}}),
    ]);
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let error = parse_response(&stream.finish(), &payload).expect_err("errors");
    assert!(format!("{error:?}").contains("boom"), "{error:?}");
}

#[tokio::test]
async fn sse_transcript_drives_the_same_assembly() {
    let transcript = concat!(
        "event: step.start\n",
        "data: {\"index\":0,\"step\":{\"type\":\"model_output\"},\"event_type\":\"step.start\"}\n",
        "\n",
        "event: step.delta\n",
        "data: {\"index\":0,\"delta\":{\"text\":\"hi\",\"type\":\"text\"},\"event_type\":\"step.delta\"}\n",
        "\n",
        "event: step.stop\n",
        "data: {\"index\":0,\"event_type\":\"step.stop\"}\n",
        "\n",
        "event: interaction.completed\n",
        "data: {\"interaction\":{\"id\":\"v1_abc\",\"status\":\"completed\"},\"event_type\":\"interaction.completed\"}\n",
        "\n",
        "event: done\n",
        "data: [DONE]\n",
        "\n",
    );
    let envelope = super::interactions::consume_interaction_sse(
        transcript.as_bytes(),
        None,
        interactions_dialect(),
    )
    .await
    .expect("stream parses");
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let result = parse_response(&envelope, &payload).expect("parses");
    assert_eq!(result.text, "hi");
    assert_eq!(result.telemetry.request_id.as_deref(), Some("v1_abc"));
}

#[tokio::test]
async fn sse_transcript_rejects_eof_before_interaction_completion() {
    let transcript = concat!(
        "event: interaction.created\n",
        "data: {\"interaction\":{\"id\":\"v1_abc\",\"status\":\"in_progress\"},\"event_type\":\"interaction.created\"}\n",
        "\n",
        "event: step.start\n",
        "data: {\"index\":0,\"step\":{\"type\":\"model_output\"},\"event_type\":\"step.start\"}\n",
        "\n",
        "event: step.delta\n",
        "data: {\"index\":0,\"delta\":{\"text\":\"partial\",\"type\":\"text\"},\"event_type\":\"step.delta\"}\n",
        "\n",
    );

    let error = super::interactions::consume_interaction_sse(
        transcript.as_bytes(),
        None,
        interactions_dialect(),
    )
    .await
    .expect_err("an incomplete interaction stream must not become a response");

    let failure = error
        .provider_stream_failure()
        .expect("an incomplete stream must report a typed failure");
    assert_eq!(failure.provider, "gemini");
    assert_eq!(
        failure.reason,
        crate::value::ProviderStreamFailureReason::PrematureEof
    );
    assert_eq!(failure.phase, crate::value::ProviderStreamPhase::Streaming);
    assert!(
        failure.partial,
        "the text delta must be recorded as partial"
    );
    assert!(
        error
            .to_string()
            .contains("stream ended before interaction.completed"),
        "{error:?}"
    );
}

#[tokio::test]
async fn stream_parser_rejects_a_mismatched_dialect_contract() {
    let dialect = crate::llm::api::DialectContract::new(
        crate::llm::capabilities::WireDialect::OpenAiCompat,
        None,
    );
    let error = super::interactions::consume_interaction_sse(b"".as_slice(), None, dialect)
        .await
        .expect_err("an OpenAI contract must not decode Gemini events");
    assert!(error.to_string().contains("mismatched dialect"));
}

#[tokio::test]
async fn events_match_external_golden() {
    #[derive(serde::Deserialize)]
    struct GoldenResult {
        text: String,
        input_tokens: i64,
        output_tokens: i64,
        stop_reason: String,
    }
    #[derive(serde::Deserialize)]
    struct Golden {
        wire_events: String,
        result: GoldenResult,
    }

    let golden: Golden = serde_json::from_str(include_str!(
        "../../testdata/dialects/gemini_interactions.json"
    ))
    .expect("valid interactions golden");
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
    let envelope = super::interactions::consume_interaction_sse(
        golden.wire_events.as_bytes(),
        Some(delta_tx),
        interactions_dialect(),
    )
    .await
    .expect("golden stream parses");
    let payload = gemini_payload(MODEL, ThinkingConfig::Disabled);
    let result = parse_response(&envelope, &payload).expect("golden response parses");

    assert_eq!(result.text, golden.result.text);
    assert_eq!(result.input_tokens, golden.result.input_tokens);
    assert_eq!(result.output_tokens, golden.result.output_tokens);
    assert_eq!(
        result.stop_reason.as_deref(),
        Some(golden.result.stop_reason.as_str())
    );
    assert_eq!(delta_rx.recv().await.as_deref(), Some("hello back"));
}
