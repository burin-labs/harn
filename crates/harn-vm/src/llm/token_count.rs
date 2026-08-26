use tiktoken_rs::tokenizer::{get_tokenizer, Tokenizer};

const TIKTOKEN_IDENTITY_PREFIX: &str = "tiktoken:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExactToken {
    pub(crate) id: u32,
    pub(crate) tokenizer: String,
    pub(crate) bytes: Vec<u8>,
}

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

pub(crate) fn exact_tokenizer_identity_for_model(model: &str) -> Result<String, String> {
    let info = tokenizer_info_for_model(model);
    if !info.exact {
        return Err(format!(
            "model `{model}` has no exact local tokenizer; `{}` is only a token-count estimate",
            info.source.as_str()
        ));
    }
    let encoder = info
        .encoder
        .ok_or_else(|| format!("model `{model}` has no exact local tokenizer"))?;
    Ok(format!("{TIKTOKEN_IDENTITY_PREFIX}{encoder}"))
}

pub(crate) fn tokenize_exact(text: &str, model: &str) -> Result<Vec<ExactToken>, String> {
    let info = tokenizer_info_for_model(model);
    if !info.exact {
        return Err(format!(
            "model `{model}` has no exact local tokenizer; approximate token counts cannot create token references"
        ));
    }
    let tokenizer = info
        .tokenizer
        .ok_or_else(|| format!("model `{model}` has no exact local tokenizer"))?;
    let identity = format!("{TIKTOKEN_IDENTITY_PREFIX}{}", encoder_name(tokenizer));
    let bpe = tiktoken_rs::bpe_for_tokenizer(tokenizer)
        .map_err(|error| format!("failed to load tokenizer `{identity}`: {error}"))?;
    bpe.encode_with_special_tokens(text)
        .into_iter()
        .map(|id| {
            let bytes = bpe
                .decode_bytes(&[id])
                .map_err(|error| format!("failed to decode token {id}: {error}"))?;
            Ok(ExactToken {
                id,
                tokenizer: identity.clone(),
                bytes,
            })
        })
        .collect()
}

pub(crate) fn detokenize_exact(tokenizer: &str, ids: &[u32]) -> Result<String, String> {
    let tokenizer_kind = tokenizer_from_identity(tokenizer).ok_or_else(|| {
        format!(
            "unknown tokenizer identity `{tokenizer}`; expected `{TIKTOKEN_IDENTITY_PREFIX}<encoder>`"
        )
    })?;
    let bpe = tiktoken_rs::bpe_for_tokenizer(tokenizer_kind)
        .map_err(|error| format!("failed to load tokenizer `{tokenizer}`: {error}"))?;
    bpe.decode(ids)
        .map_err(|error| format!("tokens are not valid UTF-8 text: {error}"))
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

fn tokenizer_from_identity(identity: &str) -> Option<Tokenizer> {
    match identity.strip_prefix(TIKTOKEN_IDENTITY_PREFIX)? {
        "o200k_harmony" => Some(Tokenizer::O200kHarmony),
        "o200k_base" => Some(Tokenizer::O200kBase),
        "cl100k_base" => Some(Tokenizer::Cl100kBase),
        "p50k_base" => Some(Tokenizer::P50kBase),
        "r50k_base" => Some(Tokenizer::R50kBase),
        "p50k_edit" => Some(Tokenizer::P50kEdit),
        "gpt2" => Some(Tokenizer::Gpt2),
        _ => None,
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

    #[test]
    fn exact_tokens_round_trip_with_stable_tokenizer_identity() {
        let tokens = tokenize_exact("hello, 世界", "gpt-4o").expect("exact tokenization");
        assert!(!tokens.is_empty());
        assert!(tokens
            .iter()
            .all(|token| token.tokenizer == "tiktoken:o200k_base"));
        let ids = tokens.iter().map(|token| token.id).collect::<Vec<_>>();
        assert_eq!(
            detokenize_exact("tiktoken:o200k_base", &ids).expect("exact decoding"),
            "hello, 世界"
        );
    }

    #[test]
    fn approximations_cannot_mint_token_references() {
        let error = tokenize_exact("hello", "claude-sonnet-4-20250514")
            .expect_err("Claude token counting is approximate");
        assert!(error.contains("cannot create token references"));
    }
}
