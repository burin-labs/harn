use super::{clear_user_overrides, lookup};

#[test]
fn openai_codex_models_are_responses_only() {
    clear_user_overrides();
    for model in [
        "gpt-5-codex",
        "gpt-5.1-codex",
        "gpt-5.1-codex-max",
        "gpt-5.1-codex-mini",
        "gpt-5.2-codex",
        "gpt-5.3-codex",
    ] {
        let caps = lookup("openai", model);
        assert!(
            caps.chat_completions_unsupported,
            "{model} must be flagged responses-only"
        );
        assert!(caps.responses_api, "{model} must advertise responses_api");
        assert!(
            caps.requires_completion_tokens,
            "{model} is a reasoning model"
        );
    }
    assert!(!lookup("openai", "gpt-5.5").chat_completions_unsupported);
    assert!(!lookup("openrouter", "openai/gpt-5.2-codex").chat_completions_unsupported);
    assert!(!lookup("zai", "glm-5.2").chat_completions_unsupported);
}
