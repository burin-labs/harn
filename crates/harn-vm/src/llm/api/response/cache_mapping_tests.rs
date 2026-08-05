use super::extract_cache_read_tokens;

#[test]
fn cache_read_tokens_cover_anthropic_and_openai_chat_shapes() {
    let anthropic = serde_json::json!({
        "input_tokens": 200,
        "cache_read_input_tokens": 10_000,
        "cache_creation_input_tokens": 0
    });
    assert_eq!(extract_cache_read_tokens(&anthropic), 10_000);

    let openai = serde_json::json!({
        "prompt_tokens": 10_200,
        "prompt_tokens_details": {
            "cached_tokens": 10_000
        }
    });
    assert_eq!(extract_cache_read_tokens(&openai), 10_000);
}
