use super::*;
use serde_json::{json, Value};

fn capabilities(style: CacheBreakpointStyle) -> Capabilities {
    let mut caps = Capabilities::default();
    caps.prompt_caching = true;
    caps.cache_breakpoint_style = style;
    caps
}

fn request() -> LlmRequestPayload {
    LlmRequestPayload {
        cache: true,
        messages: vec![json!({"role":"user","content":"a new session"})],
        native_tools: Some(vec![
            json!({"name":"first","input_schema":{"type":"object"}}),
            json!({"name":"last","input_schema":{"type":"object"}}),
        ]),
        ..Default::default()
    }
}

fn marker_count(value: &Value) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(object.contains_key("cache_control"))
                + object.values().map(marker_count).sum::<usize>()
        }
        Value::Array(values) => values.iter().map(marker_count).sum(),
        _ => 0,
    }
}

#[test]
fn top_level_with_native_tools_resolves_a_truthful_index() {
    let request = request();
    let resolved =
        PromptCacheBreakpoint::resolve(&request, &capabilities(CacheBreakpointStyle::TopLevel));
    assert_eq!(
        resolved,
        PromptCacheBreakpoint::Placed {
            style: BreakpointPlacement::ToolsAndTopLevel { tool_index: 1 },
        }
    );
    assert_eq!(
        serde_json::to_value(resolved).unwrap(),
        json!({
            "outcome":"placed","style":{"tools_and_top_level":{"tool_index":1}}
        })
    );
    let mut body = json!({"tools":request.native_tools,"messages":request.messages});
    let marker = json!({"type":"ephemeral","ttl":"1h"});
    apply_prompt_cache_breakpoint(&mut body, resolved, marker.clone());
    assert_eq!(marker_count(&body), 2);
    assert_eq!(body["cache_control"], marker);
    assert_eq!(body["tools"][1]["cache_control"], marker);
    assert!(body["tools"][0].get("cache_control").is_none());
}

#[test]
fn absent_and_empty_native_tools_keep_ordinary_top_level() {
    for tools in [None, Some(vec![])] {
        let mut request = request();
        request.native_tools = tools;
        assert_eq!(
            PromptCacheBreakpoint::resolve(&request, &capabilities(CacheBreakpointStyle::TopLevel)),
            PromptCacheBreakpoint::Placed {
                style: BreakpointPlacement::TopLevel
            }
        );
    }
}

#[test]
fn cache_off_unsupported_automatic_and_last_block_keep_their_contracts() {
    let mut request = request();
    let mut caps = capabilities(CacheBreakpointStyle::TopLevel);
    request.cache = false;
    assert_eq!(
        PromptCacheBreakpoint::resolve(&request, &caps),
        PromptCacheBreakpoint::NotRequested
    );
    request.cache = true;
    caps.prompt_caching = false;
    assert_eq!(
        PromptCacheBreakpoint::resolve(&request, &caps),
        PromptCacheBreakpoint::Unsupported
    );
    assert_eq!(
        PromptCacheBreakpoint::resolve(&request, &capabilities(CacheBreakpointStyle::None)),
        PromptCacheBreakpoint::Automatic
    );
    assert_eq!(
        PromptCacheBreakpoint::resolve(&request, &capabilities(CacheBreakpointStyle::LastBlock)),
        PromptCacheBreakpoint::Placed {
            style: BreakpointPlacement::LastBlock
        }
    );
}

#[test]
fn explicit_caller_marker_wins_but_schema_named_field_is_data() {
    let caps = capabilities(CacheBreakpointStyle::TopLevel);
    let mut request = request();
    request.native_tools.as_mut().unwrap()[0]["input_schema"]["properties"] =
        json!({"cache_control":{"type":"string"}});
    assert!(matches!(
        PromptCacheBreakpoint::resolve(&request, &caps),
        PromptCacheBreakpoint::Placed {
            style: BreakpointPlacement::ToolsAndTopLevel { tool_index: 1 }
        }
    ));
    request.native_tools.as_mut().unwrap()[0]["cache_control"] = json!({"type":"ephemeral"});
    let resolved = PromptCacheBreakpoint::resolve(&request, &caps);
    assert_eq!(resolved, PromptCacheBreakpoint::DeferredToExistingMarker);
    let mut body = json!({"tools":request.native_tools,"messages":request.messages});
    let original = body.clone();
    apply_prompt_cache_breakpoint(&mut body, resolved, json!({"type":"ephemeral"}));
    assert_eq!(body, original);
    assert_eq!(
        marker_count(&body),
        2,
        "one caller marker plus the schema property, no Harn markers"
    );
    assert!(body.get("cache_control").is_none());
}

fn encode(request: &LlmRequestPayload, caps: &Capabilities) -> Value {
    if request.provider == "anthropic" {
        crate::wire::anthropic::encode_request(
            request,
            &crate::wire::anthropic::AnthropicWireContext {
                capabilities: caps,
                wire_model: &request.model,
                prompt_cache: PromptCacheBreakpoint::resolve(request, caps),
                tool_search_extensions: true,
            },
        )
        .body
    } else {
        crate::wire::openai::encode_request(
            request,
            &crate::wire::openai::OpenAiWireContext {
                capabilities: caps,
                wire_model: &request.model,
                may_shape_reasoning: false,
                tool_search_extensions: true,
                fast_request: None,
            },
        )
        .body
    }
}

#[test]
fn both_adapters_mark_the_final_native_tool_and_automatic_conversation() {
    for provider in ["anthropic", "openrouter"] {
        let mut request = request();
        request.provider = provider.to_string();
        request.model = "fixture".to_string();
        if provider == "openrouter" {
            request.native_tools = Some(vec![
                json!({"type":"function","function":{"name":"first","parameters":{"type":"object"}}}),
                json!({"type":"function","function":{"name":"last","parameters":{"type":"object"}}}),
            ]);
        } else {
            request.provider_tools =
                vec![json!({"type":"web_search_20250305","name":"web_search"})];
        }
        let body = encode(&request, &capabilities(CacheBreakpointStyle::TopLevel));
        assert_eq!(marker_count(&body), 2, "{provider}: {body}");
        assert_eq!(body["cache_control"], json!({"type":"ephemeral"}));
        assert_eq!(body["tools"][1]["cache_control"], body["cache_control"]);
        assert_eq!(
            if provider == "anthropic" {
                &body["tools"][1]["name"]
            } else {
                &body["tools"][1]["function"]["name"]
            },
            "last"
        );
        assert!(body["tools"][0].get("cache_control").is_none());
        if provider == "anthropic" {
            assert_eq!(body["tools"][2]["name"], "web_search");
            assert!(body["tools"][2].get("cache_control").is_none());
        }
        request.native_tools.as_mut().unwrap()[0]["cache_control"] = json!({"type":"ephemeral"});
        let deferred = encode(&request, &capabilities(CacheBreakpointStyle::TopLevel));
        assert_eq!(marker_count(&deferred), 1, "{provider}: {deferred}");
        assert!(deferred.get("cache_control").is_none());
        assert!(deferred["tools"][1].get("cache_control").is_none());
    }
}

#[test]
fn trailing_deferred_tools_never_receive_automatic_breakpoints() {
    for provider in ["anthropic", "openrouter"] {
        let mut request = request();
        request.provider = provider.to_string();
        request.model = "fixture".to_string();
        if provider == "openrouter" {
            request.native_tools = Some(vec![
                json!({"type":"function","function":{"name":"first","parameters":{"type":"object"}}}),
                json!({"type":"function","function":{"name":"last","parameters":{"type":"object"}}}),
            ]);
        }
        request.native_tools.as_mut().unwrap()[1]["defer_loading"] = json!(true);
        let caps = capabilities(CacheBreakpointStyle::TopLevel);
        assert_eq!(
            PromptCacheBreakpoint::resolve(&request, &caps),
            PromptCacheBreakpoint::Placed {
                style: BreakpointPlacement::ToolsAndTopLevel { tool_index: 0 }
            }
        );
        let body = encode(&request, &caps);
        assert_eq!(marker_count(&body), 2, "{provider}: {body}");
        assert_eq!(body["tools"][0]["cache_control"], body["cache_control"]);
        assert_eq!(
            if provider == "anthropic" {
                &body["tools"][0]["name"]
            } else {
                &body["tools"][0]["function"]["name"]
            },
            "first"
        );
        assert!(body["tools"][1].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["defer_loading"], true);
        if provider == "openrouter" {
            request.native_tools.as_mut().unwrap()[1]["defer_loading"] = json!(false);
            request.native_tools.as_mut().unwrap()[1]["function"]["defer_loading"] = json!(true);
            assert_eq!(
                PromptCacheBreakpoint::resolve(&request, &caps),
                PromptCacheBreakpoint::Placed {
                    style: BreakpointPlacement::ToolsAndTopLevel { tool_index: 0 }
                }
            );
            let nested = encode(&request, &caps);
            assert_eq!(nested["tools"][1]["function"]["defer_loading"], true);
            assert!(nested["tools"][1].get("cache_control").is_none());
            assert_eq!(nested["tools"][0]["cache_control"], nested["cache_control"]);
            assert_eq!(marker_count(&nested), 2);
        }
        request.native_tools.as_mut().unwrap()[0]["defer_loading"] = json!(true);
        assert_eq!(
            PromptCacheBreakpoint::resolve(&request, &caps),
            PromptCacheBreakpoint::Placed {
                style: BreakpointPlacement::TopLevel
            }
        );
        let all_deferred = encode(&request, &caps);
        assert_eq!(marker_count(&all_deferred), 1, "{provider}: {all_deferred}");
        assert_eq!(all_deferred["cache_control"], json!({"type":"ephemeral"}));
        assert!(all_deferred["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool.get("cache_control").is_none()));
    }
}
