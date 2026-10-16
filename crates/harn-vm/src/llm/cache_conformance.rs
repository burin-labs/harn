//! Prompt-cache conformance using the active runtime capability snapshot.

#[doc(inline)]
pub use harn_llm::cache_conformance::*;

/// Resolve support from the active provider configuration and measurements.
pub fn prompt_cache_support(provider: &str, model: &str) -> PromptCacheSupport {
    let provider = provider.trim();
    let model = model.trim();
    let caps = if provider.is_empty() || provider.eq_ignore_ascii_case("auto") || model.is_empty() {
        Default::default()
    } else {
        super::capabilities::lookup(provider, model)
    };
    prompt_cache_support_with_capabilities(provider, model, &caps)
}

/// Parse saved runs once, then classify using the active runtime capabilities.
pub fn classify_cache_conformance_fixture(
    provider: impl Into<String>,
    model: impl Into<String>,
    raw: &str,
) -> Result<CacheConformanceReport, String> {
    let fixture = CacheConformanceFixture::parse(provider, model, raw)?;
    let support = prompt_cache_support(fixture.provider(), fixture.model());
    Ok(fixture.classify(support))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_runs_use_active_capability_overrides_and_fixture_route_identity() {
        struct Restore(Option<super::super::capabilities::CapabilitiesFile>);
        impl Drop for Restore {
            fn drop(&mut self) {
                super::super::capabilities::swap_user_overrides(self.0.take());
            }
        }
        let _restore = Restore(super::super::capabilities::swap_user_overrides(None));
        let raw = r#"{"provider":"openai","model":"gpt-5.4","runs":[{"input_tokens":2048,"output_tokens":1,"cache_read_tokens":0,"cache_write_tokens":0}]}"#;
        let baseline = classify_cache_conformance_fixture("", "", raw).unwrap();
        assert_eq!(baseline.support.supported, Some(true));
        assert_eq!(baseline.verdict, CacheVerdict::InsufficientRuns);
        super::super::capabilities::set_user_overrides_toml(
            r#"
[[provider.openai]]
model_match = "gpt-5.4"
prompt_caching = false
"#,
        )
        .unwrap();
        let overridden = classify_cache_conformance_fixture("", "", raw).unwrap();
        assert_eq!(overridden.provider, "openai");
        assert_eq!(overridden.model, "gpt-5.4");
        assert_eq!(overridden.support.supported, Some(false));
        assert_eq!(overridden.verdict, CacheVerdict::UnsupportedZero);
        assert_eq!(prompt_cache_support("auto", "").supported, None);
    }
}
