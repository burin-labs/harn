use super::extract_cache_counts;

#[test]
fn cache_read_tokens_cover_anthropic_and_openai_chat_shapes() {
    let anthropic = serde_json::json!({
        "input_tokens": 200,
        "cache_read_input_tokens": 10_000,
        "cache_creation_input_tokens": 0
    });
    assert_eq!(
        extract_cache_counts(&anthropic).unwrap().read_tokens,
        10_000
    );

    let openai = serde_json::json!({
        "prompt_tokens": 10_200,
        "prompt_tokens_details": {
            "cached_tokens": 10_000
        }
    });
    assert_eq!(extract_cache_counts(&openai).unwrap().read_tokens, 10_000);
}

#[test]
fn cache_read_tokens_cover_together_flat_shape() {
    let together = serde_json::json!({
        "prompt_tokens": 10_200,
        "completion_tokens": 20,
        "total_tokens": 10_220,
        "cached_tokens": 10_000
    });

    assert_eq!(extract_cache_counts(&together).unwrap().read_tokens, 10_000);
}

#[test]
fn an_omitted_cache_counter_is_unreported_and_an_explicit_zero_is_reported() {
    let omitted = extract_cache_counts(&serde_json::json!({"prompt_tokens": 9})).unwrap();
    assert!(!omitted.reported.has_any());
    assert_eq!((omitted.read_tokens, omitted.write_tokens), (0, 0));

    let zero = extract_cache_counts(&serde_json::json!({
        "prompt_tokens": 9,
        "prompt_tokens_details": {"cached_tokens": 0}
    }))
    .unwrap();
    assert!(zero.reported.has_any());
    assert_eq!(zero.read_tokens, 0);
}

#[test]
fn openai_responses_parser_carries_cache_counter_presence() {
    let response = |usage: serde_json::Value| {
        serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [{
                "type": "message",
                "id": "msg_1",
                "content": [{"type": "output_text", "text": "ok"}]
            }],
            "usage": usage
        })
    };
    let omitted = super::parse_openai_responses_response(
        &response(serde_json::json!({"input_tokens": 9, "output_tokens": 5})),
        "openai",
        "gpt-5.4",
    )
    .unwrap();
    assert!(!omitted.cache_counts_reported);
    let zero = super::parse_openai_responses_response(
        &response(serde_json::json!({
            "input_tokens": 9,
            "output_tokens": 5,
            "input_tokens_details": {"cached_tokens": 0}
        })),
        "openai",
        "gpt-5.4",
    )
    .unwrap();
    assert!(zero.cache_counts_reported);
    assert_eq!(zero.cache_read_tokens, 0);
}
