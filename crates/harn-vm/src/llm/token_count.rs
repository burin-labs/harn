use tiktoken_rs::tokenizer::{get_tokenizer, Tokenizer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenCountSource {
    TiktokenExact,
    TiktokenApproximation,
    Heuristic,
}

impl TokenCountSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TokenCountSource::TiktokenExact => "tiktoken",
            TokenCountSource::TiktokenApproximation => "tiktoken_approximation",
            TokenCountSource::Heuristic => "heuristic",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TokenizerInfo {
    pub(crate) encoder: Option<&'static str>,
    pub(crate) tokenizer: Option<Tokenizer>,
    pub(crate) model_family: &'static str,
    pub(crate) source: TokenCountSource,
    pub(crate) exact: bool,
    pub(crate) known_model_family: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TokenCountEstimate {
    pub(crate) tokens: i64,
    pub(crate) info: TokenizerInfo,
}

pub(crate) fn tokenizer_info_for_model(model: &str) -> TokenizerInfo {
    if let Some(tokenizer) = openai_tokenizer(model) {
        return TokenizerInfo {
            encoder: Some(encoder_name(tokenizer)),
            tokenizer: Some(tokenizer),
            model_family: "openai",
            source: TokenCountSource::TiktokenExact,
            exact: true,
            known_model_family: true,
        };
    }

    let lower = model.to_ascii_lowercase();
    if is_claude_model(&lower) {
        return approximate_info("anthropic");
    }
    if is_gemini_model(&lower) {
        return approximate_info("gemini");
    }

    TokenizerInfo {
        encoder: None,
        tokenizer: None,
        model_family: "unknown",
        source: TokenCountSource::Heuristic,
        exact: false,
        known_model_family: false,
    }
}

pub(crate) fn tiktoken_count_text(text: &str, model: &str) -> Result<TokenCountEstimate, String> {
    let info = tokenizer_info_for_model(model);
    let Some(tokenizer) = info.tokenizer else {
        return Err(format!("no tiktoken encoder for model `{model}`"));
    };
    let bpe = tiktoken_rs::bpe_for_tokenizer(tokenizer)
        .map_err(|error| format!("failed to load tiktoken encoder: {error}"))?;
    Ok(TokenCountEstimate {
        tokens: bpe.count_with_special_tokens(text) as i64,
        info,
    })
}

pub(crate) fn estimate_text_tokens(text: &str, model: Option<&str>) -> TokenCountEstimate {
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        if let Ok(count) = tiktoken_count_text(text, model) {
            return count;
        }
    }

    TokenCountEstimate {
        tokens: heuristic_text_tokens(text),
        info: TokenizerInfo {
            encoder: None,
            tokenizer: None,
            model_family: "unknown",
            source: TokenCountSource::Heuristic,
            exact: false,
            known_model_family: false,
        },
    }
}

fn approximate_info(model_family: &'static str) -> TokenizerInfo {
    TokenizerInfo {
        encoder: Some("cl100k_base"),
        tokenizer: Some(Tokenizer::Cl100kBase),
        model_family,
        source: TokenCountSource::TiktokenApproximation,
        exact: false,
        known_model_family: true,
    }
}

fn openai_tokenizer(model: &str) -> Option<Tokenizer> {
    get_tokenizer(model).or_else(|| {
        model
            .rsplit_once('/')
            .and_then(|(_, suffix)| get_tokenizer(suffix))
    })
}

fn is_claude_model(lower: &str) -> bool {
    lower.contains("claude")
}

fn is_gemini_model(lower: &str) -> bool {
    lower.contains("gemini")
}

fn encoder_name(tokenizer: Tokenizer) -> &'static str {
    match tokenizer {
        Tokenizer::O200kHarmony => "o200k_harmony",
        Tokenizer::O200kBase => "o200k_base",
        Tokenizer::Cl100kBase => "cl100k_base",
        Tokenizer::P50kBase => "p50k_base",
        Tokenizer::R50kBase => "r50k_base",
        Tokenizer::P50kEdit => "p50k_edit",
        Tokenizer::Gpt2 => "gpt2",
    }
}

fn heuristic_text_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count() as f64;
    let divisor = if contains_cjk(text) {
        1.0
    } else if looks_like_code_or_markdown(text) {
        3.5
    } else {
        4.0
    };
    (chars / divisor).ceil() as i64
}

fn contains_cjk(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3040..=0x30ff | 0x3400..=0x9fff | 0xac00..=0xd7af
        )
    })
}

fn looks_like_code_or_markdown(text: &str) -> bool {
    text.contains("```")
        || text.contains("::")
        || text.contains("=>")
        || text.contains("->")
        || text.contains('{')
        || text.contains('}')
        || text.contains(';')
        || text.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("- ")
                || trimmed.starts_with("* ")
                || trimmed.starts_with("# ")
                || trimmed.starts_with("## ")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_models_report_exact_encoder() {
        let info = tokenizer_info_for_model("gpt-4o");
        assert_eq!(info.encoder, Some("o200k_base"));
        assert_eq!(info.model_family, "openai");
        assert!(info.exact);
    }

    #[test]
    fn provider_prefixed_openai_models_resolve_suffix() {
        let info = tokenizer_info_for_model("openai/gpt-4");
        assert_eq!(info.encoder, Some("cl100k_base"));
        assert!(info.exact);
    }

    #[test]
    fn claude_and_gemini_are_labeled_approximations() {
        for model in ["claude-sonnet-4-20250514", "gemini-2.5-pro"] {
            let info = tokenizer_info_for_model(model);
            assert_eq!(info.encoder, Some("cl100k_base"));
            assert_eq!(info.source, TokenCountSource::TiktokenApproximation);
            assert!(!info.exact);
            assert!(info.known_model_family);
        }
    }

    #[test]
    fn unknown_models_use_heuristic_fallback() {
        let estimate = estimate_text_tokens("hello world", Some("local-qwen"));
        assert_eq!(estimate.tokens, 3);
        assert_eq!(estimate.info.source, TokenCountSource::Heuristic);
        assert_eq!(estimate.info.encoder, None);
    }
}
