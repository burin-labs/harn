//! Verify the physical endpoint for newly supported direct token contracts.

pub(super) fn verified(provider: &str) -> bool {
    let expected = match provider {
        "fireworks" => "https://api.fireworks.ai/inference/v1",
        "groq" => "https://api.groq.com/openai/v1",
        _ => return true, // Preserve the pre-existing OpenAI/Anthropic contract.
    };
    let Some(config) = crate::llm_config::provider_config(provider) else {
        return false;
    };
    if config.protocol.is_some()
        || config.managed_supply.is_some()
        || !matches!(
            config.chat_api_adapter,
            None | Some(crate::llm_config::ChatApiAdapter::ModelDefault)
        )
        || config.chat_endpoint != "/chat/completions"
    {
        return false;
    }
    let base = crate::llm_config::resolve_base_url(&config);
    if base.trim_end_matches('/') == expected {
        return true;
    }
    #[cfg(test)]
    if FIXTURE_ENDPOINT.with(std::cell::Cell::get) {
        // Only unit-test binaries contain this seam. It cannot grant a live
        // environment variable or admit an arbitrary remote billing gateway.
        return url::Url::parse(&base).is_ok_and(|url| {
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/v1"
        });
    }
    false
}

#[cfg(test)]
thread_local! {
    static FIXTURE_ENDPOINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) struct FixtureEndpointGuard(bool);

#[cfg(test)]
impl Drop for FixtureEndpointGuard {
    fn drop(&mut self) {
        FIXTURE_ENDPOINT.with(|slot| slot.set(self.0));
    }
}

#[cfg(test)]
pub(crate) fn allow_fixture_endpoint() -> FixtureEndpointGuard {
    FixtureEndpointGuard(FIXTURE_ENDPOINT.with(|slot| slot.replace(true)))
}
