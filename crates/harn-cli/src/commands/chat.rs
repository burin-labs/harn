//! `harn chat` dispatches to the embedded `cli/chat.harn` script.
//!
//! The shim owns exactly two things the script cannot: deciding which model
//! "whatever Harn has live" means right now, and checking that the model is
//! reachable before dropping the user at a prompt that would fail on their
//! first message. Conversation behavior, slash commands, and stats rendering
//! all live in the script.

use crate::cli::ChatArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

pub(crate) async fn run(args: ChatArgs) -> i32 {
    let routing = match resolve_routing(args.model.as_deref(), args.provider.as_deref()) {
        Ok(routing) => routing,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };

    if let Some(message) = unreachable_provider_message(&routing).await {
        eprintln!("{message}");
        return 1;
    }

    let _provider = ScopedEnvVar::set("HARN_CHAT_PROVIDER", &routing.provider);
    let _model = ScopedEnvVar::set("HARN_CHAT_MODEL", &routing.model);
    let _label = ScopedEnvVar::set("HARN_CHAT_LABEL", &routing.label);
    let _endpoint = ScopedEnvVar::set("HARN_CHAT_ENDPOINT", &routing.endpoint);
    let _stats = ScopedEnvVar::set("HARN_CHAT_STATS", args.stats_mode());
    let _system = args
        .system
        .as_deref()
        .map(|value| ScopedEnvVar::set("HARN_CHAT_SYSTEM", value));

    dispatch::dispatch_to_interactive_embedded_script("chat", Vec::new()).await
}

pub(crate) struct ChatRouting {
    pub provider: String,
    pub model: String,
    /// What to call the model in the banner: the alias the user knows it by
    /// when there is one, otherwise the resolved id.
    pub label: String,
    /// Where requests will actually go. Shown before the first prompt so the
    /// route is never an ambient fact the user has to infer.
    pub endpoint: String,
}

/// The endpoint requests will actually be sent to, as the provider catalog
/// resolves it (including any `*_BASE_URL` override).
fn resolve_endpoint(provider: &str) -> String {
    harn_vm::llm_config::provider_config(provider)
        .map(|pdef| harn_vm::llm_config::resolve_base_url(&pdef))
        .unwrap_or_default()
}

/// Assert that the route we resolved is the route that was asked for.
///
/// Two of chat's four precedence layers are ambient — the stored local
/// selection and the environment — so an explicit request being silently
/// overridden by one of them is a real failure mode, and one that would look
/// like a working session against the wrong model. A gate that reads ambient
/// configuration without checking the result is not a gate, so this compares
/// what was requested against what was resolved and refuses to start on a
/// mismatch rather than quietly talking to something else.
fn assert_requested_route_honored(
    routing: &ChatRouting,
    requested_model: Option<&str>,
    requested_provider: Option<&str>,
) -> Result<(), String> {
    if let Some(requested) = requested_provider.map(str::trim).filter(|v| !v.is_empty()) {
        if routing.provider != requested {
            return Err(format!(
                "asked for provider {requested} but resolved to {}. \
                 Refusing to start rather than talk to a different provider \
                 than you asked for.",
                routing.provider
            ));
        }
    }
    if let Some(requested) = requested_model.map(str::trim).filter(|v| !v.is_empty()) {
        let resolved = harn_vm::llm_config::resolve_model_info(requested);
        if routing.model != resolved.id {
            return Err(format!(
                "asked for model {requested} (which resolves to {}) but the \
                 session resolved to {}. Refusing to start rather than talk to \
                 a different model than you asked for.",
                resolved.id, routing.model
            ));
        }
    }
    Ok(())
}

/// Decide which model this session talks to.
///
/// Precedence, most explicit first: an argument the user just typed, then the
/// active `harn local switch` selection, then the configured provider default.
/// The local selection is consulted *here* rather than in global provider
/// resolution so a stale switch can never silently re-route commands that
/// never asked for it.
pub(crate) fn resolve_routing(
    model: Option<&str>,
    provider: Option<&str>,
) -> Result<ChatRouting, String> {
    let routing = resolve_routing_inner(model, provider)?;
    assert_requested_route_honored(&routing, model, provider)?;
    Ok(routing)
}

fn resolve_routing_inner(
    model: Option<&str>,
    provider: Option<&str>,
) -> Result<ChatRouting, String> {
    if let Some(selector) = model.map(str::trim).filter(|value| !value.is_empty()) {
        let resolved = harn_vm::llm_config::resolve_model_info(selector);
        let provider = provider
            .map(str::to_string)
            .unwrap_or_else(|| resolved.provider.clone());
        return Ok(ChatRouting {
            endpoint: resolve_endpoint(&provider),
            provider,
            model: resolved.id,
            label: resolved.alias.unwrap_or_else(|| selector.to_string()),
        });
    }

    let base_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Some(selection) = harn_vm::local_selection::read_selection(&base_dir)? {
        let provider = provider
            .map(str::to_string)
            .unwrap_or_else(|| selection.provider.clone());
        return Ok(ChatRouting {
            // The selection records the endpoint it was launched against; a
            // provider override means that record no longer applies.
            endpoint: if provider == selection.provider {
                selection.base_url.clone()
            } else {
                resolve_endpoint(&provider)
            },
            provider,
            label: selection
                .alias
                .clone()
                .unwrap_or_else(|| selection.model.clone()),
            model: selection.model,
        });
    }

    if harn_vm::llm::available_provider_names().is_empty() {
        return Err(crate::commands::doctor::no_credentials_hint());
    }

    let provider = provider
        .map(str::to_string)
        .unwrap_or_else(harn_vm::llm_config::default_provider);
    let model = harn_vm::llm_config::default_model_for_provider(&provider);
    Ok(ChatRouting {
        endpoint: resolve_endpoint(&provider),
        label: model.clone(),
        provider,
        model,
    })
}

/// Check the route before the user types anything, so an unreachable local
/// server is reported as itself instead of surfacing as a failed first
/// message. Only definite verdicts block: a provider that cannot be probed at
/// all is not evidence of a problem.
async fn unreachable_provider_message(routing: &ChatRouting) -> Option<String> {
    use harn_vm::llm::readiness::ReadinessStatus;

    let readiness = harn_vm::llm::readiness::probe_provider_readiness(
        &routing.provider,
        Some(routing.model.as_str()),
        None,
    )
    .await;
    if readiness.ok {
        return None;
    }
    if !matches!(
        readiness.status,
        ReadinessStatus::Unreachable | ReadinessStatus::ModelMissing
    ) {
        return None;
    }
    // Point at the local-runtime commands only when they are the fix. Telling
    // someone whose cloud provider is unreachable to run `harn local launch`
    // sends them somewhere that cannot help.
    if harn_vm::llm_config::provider_is_self_hosted(&routing.provider) {
        return Some(format!(
            "{}\n`harn local list` shows which local models are available; \
             `harn local launch` starts one.",
            readiness.message
        ));
    }
    Some(readiness.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ChatArgs;

    fn args(verbose: bool, no_stats: bool) -> ChatArgs {
        ChatArgs {
            model: None,
            provider: None,
            system: None,
            verbose,
            no_stats,
        }
    }

    #[test]
    fn stats_default_to_one_compact_line() {
        assert_eq!(args(false, false).stats_mode(), "compact");
        assert_eq!(args(true, false).stats_mode(), "verbose");
        assert_eq!(args(false, true).stats_mode(), "off");
    }

    #[test]
    fn an_explicit_model_wins_over_the_local_selection() {
        let _guard = crate::tests::common::harn_state_lock::lock_harn_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::current_dir().ok();
        std::env::set_current_dir(dir.path()).expect("cwd");
        harn_vm::local_selection::write_selection(
            dir.path(),
            &harn_vm::local_selection::LocalSelection::now(
                "llamacpp",
                "local-model",
                None,
                "http://127.0.0.1:8001",
                None,
                None,
            ),
        )
        .expect("write selection");

        let routing = resolve_routing(Some("gpt-4o-mini"), None).expect("routing");
        assert_ne!(
            routing.model, "local-model",
            "an explicitly requested model must not be overridden by a stale switch"
        );

        if let Some(previous) = previous {
            let _ = std::env::set_current_dir(previous);
        }
    }

    #[test]
    fn the_local_selection_is_the_default_when_no_model_is_named() {
        let _guard = crate::tests::common::harn_state_lock::lock_harn_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::current_dir().ok();
        std::env::set_current_dir(dir.path()).expect("cwd");
        harn_vm::local_selection::write_selection(
            dir.path(),
            &harn_vm::local_selection::LocalSelection::now(
                "llamacpp",
                "qwen36-30b",
                Some("qwen36-coder".to_string()),
                "http://127.0.0.1:8001",
                None,
                None,
            ),
        )
        .expect("write selection");

        let routing = resolve_routing(None, None).expect("routing");
        assert_eq!(routing.provider, "llamacpp");
        assert_eq!(routing.model, "qwen36-30b");
        assert_eq!(
            routing.label, "qwen36-coder",
            "the banner should use the alias the user switched by"
        );

        if let Some(previous) = previous {
            let _ = std::env::set_current_dir(previous);
        }
    }

    #[test]
    fn a_route_that_does_not_match_the_request_refuses_to_start() {
        // The guard's job is to catch a resolved route that silently differs
        // from what was asked for, so drive it directly with a mismatched
        // pair rather than trusting that resolution can never produce one.
        let routing = ChatRouting {
            provider: "ollama".to_string(),
            model: "some-model".to_string(),
            label: "some-model".to_string(),
            endpoint: "http://127.0.0.1:11434".to_string(),
        };
        let error = assert_requested_route_honored(&routing, None, Some("llamacpp"))
            .expect_err("a provider mismatch must refuse to start");
        assert!(
            error.contains("llamacpp") && error.contains("ollama"),
            "the refusal should name both what was asked for and what was resolved: {error}"
        );

        assert_requested_route_honored(&routing, None, Some("ollama"))
            .expect("a matching provider is honored");
        assert_requested_route_honored(&routing, None, None)
            .expect("no explicit request means nothing to contradict");
    }

    #[test]
    fn the_banner_names_the_endpoint_the_selection_recorded() {
        let _guard = crate::tests::common::harn_state_lock::lock_harn_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::current_dir().ok();
        std::env::set_current_dir(dir.path()).expect("cwd");
        harn_vm::local_selection::write_selection(
            dir.path(),
            &harn_vm::local_selection::LocalSelection::now(
                "llamacpp",
                "qwen36-30b",
                None,
                "http://127.0.0.1:8001",
                None,
                None,
            ),
        )
        .expect("write selection");

        let routing = resolve_routing(None, None).expect("routing");
        assert_eq!(
            routing.endpoint, "http://127.0.0.1:8001",
            "the session must report where requests actually go"
        );

        if let Some(previous) = previous {
            let _ = std::env::set_current_dir(previous);
        }
    }

    #[test]
    fn an_explicit_provider_overrides_the_selection_provider() {
        let _guard = crate::tests::common::harn_state_lock::lock_harn_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::current_dir().ok();
        std::env::set_current_dir(dir.path()).expect("cwd");
        harn_vm::local_selection::write_selection(
            dir.path(),
            &harn_vm::local_selection::LocalSelection::now(
                "llamacpp",
                "qwen36-30b",
                None,
                "http://127.0.0.1:8001",
                None,
                None,
            ),
        )
        .expect("write selection");

        let routing = resolve_routing(None, Some("ollama")).expect("routing");
        assert_eq!(routing.provider, "ollama");
        assert_eq!(routing.model, "qwen36-30b");

        if let Some(previous) = previous {
            let _ = std::env::set_current_dir(previous);
        }
    }
}
