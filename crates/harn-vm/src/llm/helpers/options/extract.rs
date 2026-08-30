use super::*;
use super::{
    defaults::*, generation::*, json::*, output::*, reminders::*, routing::*, system_prompt::*,
    thinking::*, tool_search::*,
};
use crate::llm::{resolve_api_key_for_selection, ProviderSelectionSource};

/// Extract all LLM call options from the standard (prompt, system, options) args.
pub(crate) fn extract_llm_options(
    args: &[VmValue],
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    use crate::llm::api::{LlmApiMode, LlmCallOptions, ToolSearchMode, ToolSearchVariant};
    use crate::llm::provider::{provider_supports_defer_loading, provider_tool_search_variants};
    use crate::llm::tools::{extract_deferred_tool_names, vm_tools_to_native};

    let prompt = args.first().map(|a| a.display()).unwrap_or_default();
    let system = args.get(1).and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    let explicit_options = args.get(2).and_then(|a| a.as_dict()).cloned();
    // The unknown-key gate runs on the RAW caller dict, before the context
    // merge or default injection can add host-owned keys. Removed synonyms
    // and typos are hard errors here — nothing is silently dropped.
    if let Some(raw) = explicit_options.as_ref() {
        super::validate::validate_llm_option_keys(raw)?;
    }
    // Capture whether the CALLER (not injected defaults) pinned a model /
    // provider before `explicit_options` is consumed by the context merge —
    // needed to reject `models:`/`ladder:` combined with a standalone route.
    let user_pinned_route = explicit_options.as_ref().is_some_and(|raw| {
        option_is_explicitly_set(raw, "model") || option_is_explicitly_set(raw, "provider")
    });
    let caller_selected_provider = explicit_options.as_ref().is_some_and(|raw| {
        option_is_explicitly_set(raw, "provider")
            && raw
                .get("provider")
                .is_some_and(|value| !value.display().eq_ignore_ascii_case("auto"))
    });
    let caller_selected_model = explicit_options
        .as_ref()
        .is_some_and(|raw| option_is_explicitly_set(raw, "model"));
    let options = crate::llm::cost_route::merge_context_options(explicit_options);

    // If we're inside an `@step`-annotated persona function and the
    // call site didn't pin a model, inherit the step's declared model
    // and budget. The persona body stays free of "if step == X use
    // cheap model" branching.
    let mut options = options;
    apply_model_role_defaults(&mut options);
    apply_active_step_defaults(&mut options);

    // A `models:`/`ladder:` ladder owns provider/model selection, so reject a
    // standalone `model:`/`provider:` pin up front — before provider inference
    // can emit a spurious "could not infer provider" fallback warning for the
    // dead-end pin.
    let has_ladder_option = options.as_ref().is_some_and(|opts| {
        option_is_explicitly_set(opts, "models") || option_is_explicitly_set(opts, "ladder")
    });
    if has_ladder_option && user_pinned_route {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "llm_call: `models:`/`ladder:` cannot be combined with an explicit \
             `model:` or `provider:` on the same call — the ladder already \
             declares every rung's model/provider. Drop the standalone pin.",
        ))));
    }

    let mut routing_policy = crate::llm::routing::extract_routing_policy(options.as_ref())?;
    let explicit_routing_policy = routing_policy.is_some();
    // A `models:`/`ladder:` ladder and an explicit `routing:` policy both drive
    // model selection, so combining them is doubly-ambiguous. Reject it loudly
    // rather than silently ignoring the ladder (the ladder lowering below only
    // runs when no explicit routing policy is present).
    if has_ladder_option && explicit_routing_policy {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "llm_call: `models:`/`ladder:` cannot be combined with an explicit \
             `routing:` policy — both drive model selection. Use one: a ladder \
             for a linear transport-failover chain, or `routing:` for the full \
             routing policy.",
        ))));
    }
    let route_policy = parse_route_policy_option(options.as_ref())?;
    let mut provider = vm_resolve_provider(&options);
    let mut model = vm_resolve_model(&options, &provider);
    let routing_decision = resolve_route_policy(&route_policy, &provider, &model)?;
    if let Some(decision) = routing_decision.as_ref() {
        provider = decision.selected_provider.clone();
        model = decision.selected_model.clone();
    }
    // Model ladder: a `models:` (inline steps) or `ladder:` (named catalog
    // ladder) option lowers onto the first-class routing chain, so it reuses
    // the exact transport-failover classification, the routing envelope
    // trace, and the schema-retry composition rather than hand-rolling a
    // fallback loop (subsumes the copies in harn-bump-fleet / harn-cloud /
    // a downstream host). The `models:`/`ladder:` + explicit `routing:` combination is
    // already rejected above, so this `is_none()` guard is a belt-and-suspenders
    // check — the ladder never coexists with an explicit routing policy.
    if routing_policy.is_none() {
        if let Some(options_dict) = options.as_ref() {
            if let Some(ladder_policy) =
                crate::llm::routing::build_model_ladder_policy(options_dict, &provider, &model)?
            {
                // The `models:`/`ladder:` + explicit `model:`/`provider:`
                // conflict is rejected earlier (before provider inference).
                routing_policy = Some(ladder_policy);
                if let Some(first) = routing_policy
                    .as_ref()
                    .and_then(|policy| policy.chain.first())
                {
                    provider = first.provider.clone();
                    model = first.model.clone();
                }
            }
        }
    }
    let route_fallbacks = match &route_policy {
        crate::llm::api::LlmRoutePolicy::PreferenceList { .. } => routing_decision
            .as_ref()
            .map(|decision| {
                decision
                    .alternatives
                    .iter()
                    .filter(|alt| !alt.selected)
                    .map(|alt| crate::llm::api::LlmRouteFallback {
                        provider: alt.provider.clone(),
                        model: alt.model.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let fallback_chain = parse_fallback_chain_option(options.as_ref());
    // A routing_policy chain owns provider/model selection: snap the
    // base options to the first link so transcript-only consumers see
    // a sensible placeholder. The executor swaps these per attempt.
    if let Some(policy) = routing_policy.as_ref() {
        if let Some(first) = policy.chain.first() {
            provider = first.provider.clone();
            model = first.model.clone();
        }
    }
    let (capability_provider, capability_model) =
        crate::llm::managed_supply::logical_route(&provider, &model)?;
    let selection_source = if routing_policy.is_some() || routing_decision.is_some() {
        ProviderSelectionSource::RoutingPolicy
    } else if caller_selected_provider {
        ProviderSelectionSource::CallOption
    } else if crate::stdlib::process::session_env_value("HARN_LLM_PROVIDER")
        .is_some_and(|selected| selected == provider)
    {
        ProviderSelectionSource::Environment
    } else if caller_selected_model {
        ProviderSelectionSource::ModelSelection
    } else {
        crate::llm::inferred_provider_selection_source(&provider)
    };
    let caps = crate::llm::capabilities::lookup(&capability_provider, &capability_model);
    let mut api_mode = parse_api_mode_option(options.as_ref())?;
    if enforce_responses_provider_gate(api_mode, &provider) {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("api_mode: \"responses\" is not supported by provider \"{provider}\""),
        ))));
    }
    let session_id = opt_str(&options, "session_id")
        .filter(|value| !value.is_empty())
        .or_else(crate::agent_sessions::current_session_id);
    let call_stage = opt_str(&options, "_call_stage").filter(|value| !value.trim().is_empty());
    let rate_limit_consumer_id = opt_str(&options, "rate_limit_consumer_id")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| session_id.clone());
    // Mock fixture scope for this call. Consumed only when a mock provider
    // serves the request; real providers ignore it.
    let declared_call_role =
        opt_str(&options, "call_role").filter(|value| !value.trim().is_empty());
    let mock_scope = opt_str(&options, "mock_scope")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| declared_call_role.clone());
    // Provenance annotation supplied by the pipeline resolver (which resolved
    // field came from a pin vs. was inherited from the primary). Observability
    // only — emitted verbatim into the `resolved_dispatch` transcript record.
    let dispatch_provenance = options
        .as_ref()
        .and_then(|o| o.get("_dispatch_provenance"))
        .and_then(crate::llm::resolved_dispatch::DispatchProvenance::from_vm_value);
    let pending_reminders = pending_reminders_from_session(session_id.as_deref());
    let rendered_reminders = render_pending_reminders(&caps, &pending_reminders);
    let reminder_lifecycle = rendered_reminder_lifecycle(
        session_id.as_deref(),
        opt_int(&options, "_iteration").unwrap_or(0),
        &pending_reminders,
        &rendered_reminders,
    );
    let assembled_system = assemble_system_prompt(system, options.as_ref(), &rendered_reminders)?;
    let system_prompt_root = assembled_system.root();
    let context_manifest = assembled_system.manifest().clone();
    let system = assembled_system.system;
    let enforce_capability_gates = !crate::llm::mock::cli_llm_mock_replay_active()
        && !crate::llm::mock::builtin_llm_mock_active();

    // Apply providers.toml model_defaults as fallbacks for unspecified params
    // (e.g. presence_penalty=1.5 for Qwen to avoid repetition loops).
    let model_defaults =
        crate::llm_config::model_params_for_route(&capability_provider, &capability_model);
    let mut portable_option_intent: std::collections::BTreeSet<
        crate::llm::capabilities::PortableOption,
    > = options
        .as_ref()
        .map(|resolved| {
            crate::llm::capabilities::PortableOption::PRESENCE_DRIVEN
                .into_iter()
                .filter(|option| option_is_explicitly_set(resolved, option.name()))
                .collect()
        })
        .unwrap_or_default();
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(|v| v.as_float()) };
    let default_int =
        |key: &str| -> Option<i64> { model_defaults.get(key).and_then(|v| v.as_integer()) };

    let max_tokens = opt_int(&options, "max_tokens")
        .or_else(|| default_int("max_tokens"))
        .unwrap_or(16384);
    let temperature = opt_float(&options, "temperature").or_else(|| default_float("temperature"));
    let top_p = opt_float(&options, "top_p").or_else(|| default_float("top_p"));
    let top_k = opt_int(&options, "top_k").or_else(|| default_int("top_k"));
    let logprobs = parse_logprobs(options.as_ref())?;
    let logit_bias = parse_logit_bias(options.as_ref(), &capability_provider, &capability_model)?;
    let min_p = opt_float(&options, "min_p");
    let repetition_penalty = opt_float(&options, "repetition_penalty");
    let prediction = parse_prediction(options.as_ref())?;
    let verbosity = parse_verbosity(options.as_ref())?;
    let mirostat = parse_mirostat(options.as_ref())?;
    let stop = opt_str_list(&options, "stop");
    let seed = opt_int(&options, "seed");
    let frequency_penalty =
        opt_float(&options, "frequency_penalty").or_else(|| default_float("frequency_penalty"));
    let presence_penalty =
        opt_float(&options, "presence_penalty").or_else(|| default_float("presence_penalty"));
    let parallel_tool_calls = match options
        .as_ref()
        .and_then(|options| options.get("parallel_tool_calls"))
    {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Bool(value)) => Some(*value),
        Some(value) => {
            return Err(generation_option_error(
                "parallel_tool_calls",
                format!("expected bool, got {}", value.type_name()),
            ))
        }
    };
    validate_generation_ranges(
        max_tokens,
        temperature,
        top_p,
        top_k,
        min_p,
        repetition_penalty,
        frequency_penalty,
        presence_penalty,
    )?;
    for (option, selected) in [
        (
            crate::llm::capabilities::PortableOption::Logprobs,
            logprobs.is_some(),
        ),
        (
            crate::llm::capabilities::PortableOption::LogitBias,
            !logit_bias.is_empty(),
        ),
        (
            crate::llm::capabilities::PortableOption::MinP,
            min_p.is_some(),
        ),
        (
            crate::llm::capabilities::PortableOption::RepetitionPenalty,
            repetition_penalty.is_some(),
        ),
        (
            crate::llm::capabilities::PortableOption::Prediction,
            prediction.is_some(),
        ),
        (
            crate::llm::capabilities::PortableOption::Verbosity,
            verbosity.is_some(),
        ),
        (
            crate::llm::capabilities::PortableOption::Mirostat,
            mirostat.is_some(),
        ),
        (
            crate::llm::capabilities::PortableOption::ParallelToolCalls,
            parallel_tool_calls.is_some(),
        ),
    ] {
        if selected {
            portable_option_intent.insert(option);
        }
    }
    let timeout = resolve_timeout_secs(opt_int(&options, "timeout_ms"));
    let idle_timeout = opt_int(&options, "idle_timeout_ms").map(|ms| {
        if ms <= 0 {
            0
        } else {
            (ms as u64).div_ceil(1000)
        }
    });
    // Provider-side prompt caching now defaults ON for routes that support
    // it. Marking the stable system+tools+history prefix cacheable discounts
    // the re-sent prefix heavily across multi-turn agent loops and the rubric
    // grader (Anthropic ephemeral ~90% off cached input; OpenRouter passes
    // cache_control through; DeepSeek/gpt-oss cache implicitly). When the route
    // does not support caching, the default resolves to `false` so the request
    // stays byte-identical (and the strict gate below never fires on the
    // default). An explicit `cache:` value is always honoured verbatim — an
    // explicit `cache: true` on a non-supporting route still errors loudly via
    // the capability gate so misconfiguration is surfaced, and `cache: false`
    // opts out everywhere.
    let cache = match options.as_ref().and_then(|o| o.get("cache")) {
        Some(value) => value.is_truthy(),
        None => caps.prompt_caching,
    };
    if matches!(options.as_ref().and_then(|o| o.get("cache")), Some(value) if value.is_truthy()) {
        portable_option_intent.insert(crate::llm::capabilities::PortableOption::Cache);
    }
    let prompt_cache_ttl = parse_prompt_cache_ttl_option(options.as_ref())?;
    if prompt_cache_ttl.is_some() {
        portable_option_intent.insert(crate::llm::capabilities::PortableOption::PromptCacheTtl);
    }
    if prompt_cache_ttl.is_some()
        && matches!(
            options.as_ref().and_then(|o| o.get("cache")),
            Some(VmValue::Bool(false))
        )
    {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "llm_call: `prompt_cache_ttl` requires provider prompt caching; remove \
             `cache: false` or omit `prompt_cache_ttl`.",
        ))));
    }
    let stream_explicit = options
        .as_ref()
        .is_some_and(|options| options.contains_key("stream"));
    let stream = options
        .as_ref()
        .and_then(|o| o.get("stream"))
        .map(|v| v.is_truthy())
        .unwrap_or_else(|| {
            crate::stdlib::process::session_env_var("HARN_LLM_STREAM")
                .ok()
                .flatten()
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true)
        });
    let stream = normalize_generation_stream(
        stream,
        stream_explicit,
        &capability_provider,
        &capability_model,
        logprobs.is_some(),
    )?;
    let thinking = resolve_thinking_config(
        options.as_ref(),
        &model_defaults,
        &capability_provider,
        &capability_model,
        &caps,
        enforce_capability_gates,
    )?;
    let mut anthropic_beta_features = parse_anthropic_beta_features_option(
        options.as_ref(),
        &thinking,
        &capability_provider,
        &capability_model,
        enforce_capability_gates,
    )?;

    // The single `output` contract key. Providers lower this typed value
    // directly; there is no response-format/schema mirror to drift.
    let parsed_output = parse_output_option(options.as_ref())?;
    let output_format = parsed_output.format;
    if enforce_capability_gates {
        validate_output_format_supported(
            &output_format,
            &capability_provider,
            &capability_model,
            &caps,
        )?;
    }
    let output_schema = output_format.schema().cloned();
    let output_validation = parsed_output.validation;
    // Stream-abort defaults to true whenever a schema is in play, so callers
    // that expect structured output get the early-abort win automatically.
    // `output: {schema, stream_abort: false}` opts out (relying on
    // `schema_retries` for post-hoc recovery).
    let schema_stream_abort = parsed_output
        .stream_abort
        .unwrap_or_else(|| output_schema.is_some());

    // Message source precedence: options.messages > prompt.
    let messages_val = options.as_ref().and_then(|o| o.get("messages")).cloned();
    let messages = if let Some(VmValue::List(msg_list)) = &messages_val {
        vm_messages_to_json(msg_list)?
    } else {
        vec![serde_json::json!({"role": "user", "content": prompt})]
    };
    let mut messages = if opt_bool(&options, "_directives_rendered") {
        messages
    } else {
        apply_rendered_reminder_messages(messages, &rendered_reminders)
    };
    let message_lineage = crate::llm::message_lineage::take_from_messages(&mut messages);
    super::reminders::strip_directive_commit_metadata(&mut messages);
    let vision =
        opt_bool(&options, "vision") || crate::llm::content::messages_contain_images(&messages)?;
    let audio = option_is_enabled(options.as_ref(), "audio")
        || crate::llm::content::messages_contain_audio(&messages)?;
    let pdf = option_is_enabled(options.as_ref(), "pdf")
        || crate::llm::content::messages_contain_pdf(&messages)?;
    let video = option_is_enabled(options.as_ref(), "video")
        || crate::llm::content::messages_contain_videos(&messages)?;
    let uses_file_ids = crate::llm::content::messages_contain_file_ids(&messages)?;
    if enforce_capability_gates && vision && !caps.vision_supported {
        return Err(unsupported_option_error(
            "vision",
            &capability_provider,
            &capability_model,
        ));
    }
    if enforce_capability_gates && audio && !caps.audio {
        return Err(unsupported_option_error(
            "audio",
            &capability_provider,
            &capability_model,
        ));
    }
    if enforce_capability_gates && pdf && !caps.pdf {
        return Err(unsupported_option_error(
            "pdf",
            &capability_provider,
            &capability_model,
        ));
    }
    if enforce_capability_gates && video && !caps.video {
        return Err(unsupported_option_error(
            "video",
            &capability_provider,
            &capability_model,
        ));
    }
    if enforce_capability_gates && uses_file_ids && !caps.files_api_supported {
        return Err(unsupported_option_error(
            "files_api",
            &capability_provider,
            &capability_model,
        ));
    }
    if uses_file_ids && caps.message_wire_format.is_anthropic() {
        crate::llm::api::push_unique_anthropic_beta_feature(
            &mut anthropic_beta_features,
            crate::stdlib::files::ANTHROPIC_FILES_API_BETA,
        );
    }
    if vision
        && !crate::llm::provider::provider_supports_image_urls(
            &capability_provider,
            &capability_model,
        )
        && crate::llm::content::messages_contain_url_images(&messages)?
    {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "llm_call: this provider/model route requires image base64; url image content is not supported",
        ))));
    }

    let tools_val = options
        .as_ref()
        .and_then(|o| o.get("tools"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .cloned();
    let requested_tool_format = opt_str(&options, "tool_format").unwrap_or_else(|| {
        crate::llm_config::default_tool_format(&capability_model, &capability_provider)
    });
    // A non-empty reason is the explicit acknowledgment used by probes and
    // matrices to measure a catalog-marked unsafe channel deliberately. It
    // must cross this owning boundary: honoring it only in the stdlib prompt
    // resolver can otherwise pair native instructions with a request whose
    // native schemas were steered away or rejected here.
    let force_tool_format = opt_str(&options, "tool_format_override_reason")
        .is_some_and(|reason| !reason.trim().is_empty());
    // FOOTGUN-REMOVAL: a tool-bearing call must use a tool_format whose channel
    // the capability registry trusts to return parseable tool calls for this
    // route. An explicit pin (or alias) can request a channel the route is
    // known to drop silently (the DeepSeek V3.2 `native` -> unparsed DSML
    // text case); steer it to the route's safe format instead of letting the
    // calls vanish. Calls without tools keep the requested format verbatim.
    // FOOTGUN-REMOVAL: before resolving a tool_format, fail fast if this route
    // has NO viable tool channel at all (registry forbids both native and text).
    // `validate_tool_format` would pass such a combo through unchanged; on a
    // tool-bearing call that can only yield a silent empty tool stream, so name
    // the bad combo and a suggested alternative up front instead of dispatching.
    if enforce_capability_gates && tools_val.is_some() && !force_tool_format {
        if let Some(message) = crate::llm::capabilities::no_viable_tool_channel_with_caps(
            &capability_provider,
            &capability_model,
            &caps,
        ) {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                message,
            ))));
        }
    }
    let tool_format = if enforce_capability_gates && tools_val.is_some() && !force_tool_format {
        let decision = crate::llm::capabilities::validate_tool_format_with_caps(
            &capability_provider,
            &capability_model,
            &requested_tool_format,
            &caps,
        );
        if let Some(reason) = &decision.correction {
            tracing::warn!(target: "harn::llm::tool_format", "{reason}");
        }
        decision.effective
    } else {
        requested_tool_format
    };
    if enforce_capability_gates
        && tools_val.is_some()
        && tool_format == "native"
        && !caps.native_tools
        && !force_tool_format
    {
        return Err(unsupported_option_error(
            "tools",
            &capability_provider,
            &capability_model,
        ));
    }
    // harn#4743: in the text tool-call lane the model emits its call inside
    // `<tool_call>…</tool_call>` in visible content. With no stop sequence the
    // provider keeps generating past the terminator and fabricates further
    // tool-call blocks and prose the loop discards — pure wasted output (one
    // observed completion burned ~8k tokens on a 41-block fake transcript).
    // Injecting the terminator(s) as stop sequences ends the completion at the
    // first complete call; the parser already recovers a complete body whose
    // close tag the stop consumed.
    let stop = resolve_stop_sequences(
        &options,
        stop,
        &tool_format,
        caps.stop_supported,
        tools_val.is_some(),
    );
    let mut native_tools = if tool_format == "native" {
        if let Some(tools) = &tools_val {
            Some(vm_tools_to_native(
                tools,
                &capability_provider,
                &capability_model,
            )?)
        } else {
            None
        }
    } else {
        None
    };
    let mut provider_tools = parse_provider_tools_option(options.as_ref())?;
    api_mode = crate::llm::api::effective_tool_api_mode(
        api_mode,
        &provider,
        &caps,
        &thinking,
        native_tools.as_ref().is_some_and(|tools| !tools.is_empty()),
    );
    if enforce_capability_gates && !provider_tools.is_empty() && api_mode != LlmApiMode::Responses {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "provider_tools requires api_mode: \"responses\"",
        ))));
    }
    // Project the neutral `computer` function tool onto the route's native
    // computer-use surface: on `native_anthropic` / `native_openai` routes this
    // suppresses the plain function copy and injects the provider-native tool
    // into `provider_tools`. Runs AFTER the Responses-only gate above so the
    // injected native tool (which the Anthropic Messages adapter also folds
    // into its `tools` array) is not rejected on the non-Responses Anthropic
    // route. `function` / `grounded` / unset routes keep the plain tool.
    crate::llm::computer_use::project_computer_tools(&caps, &mut native_tools, &mut provider_tools);

    // tool_search option parsing: three shapes accepted.
    //   - shorthand string: "bm25" | "regex" | "hybrid" (mode: auto)
    //   - bool: true (defaults to bm25/auto), false (no tool_search)
    //   - dict: { variant, mode, strategy, always_loaded, name }
    // Unset / false / nil all leave tool_search absent — tools ship eagerly.
    let mut tool_search = parse_tool_search_option(options.as_ref())?;

    if let Some(cfg) = tool_search.as_mut() {
        // Resolve tool_search against the active provider now. Three
        // possible outcomes:
        //   - native: prepend the provider's meta-tool (Anthropic path
        //     for Claude 4.0+; OpenAI Responses-API path for GPT 5.4+).
        //   - client: leave the provider payload alone; the Harn stdlib
        //     agent loop filters deferred tools, injects the synthetic
        //     search tool, and emits client-mode events.
        //   - error: explicit native mode on a provider that cannot
        //     satisfy it.
        let native_variants =
            provider_tool_search_variants(&capability_provider, &capability_model);
        let model_based_native =
            provider_supports_defer_loading(&capability_provider, &capability_model)
                && !native_variants.is_empty();
        // Escape hatch for proxied OpenAI-compat providers whose model
        // ID Harn cannot parse. The override forces the OpenAI
        // Responses-API shape; user asserts the endpoint forwards
        // `tool_search` + `defer_loading` unchanged.
        let forced = provider_overrides_force_native(options.as_ref(), &provider);
        let provider_has_native = model_based_native || forced;
        if cfg.variant == ToolSearchVariant::Hybrid && cfg.mode == ToolSearchMode::Native {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "tool_search: variant \"hybrid\" is client-only; set mode: \"client\" or use \"bm25\"/\"regex\" for native provider tool search",
            ))));
        }
        // If the forced path is active, use OpenAI's default variants
        // so the injection below picks the right shape.
        let effective_variants: Vec<String> = if forced && native_variants.is_empty() {
            vec!["hosted".to_string(), "client".to_string()]
        } else {
            native_variants
        };
        let variant_supported = |v: &str| effective_variants.iter().any(|x| x == v);
        let resolution = match cfg.mode {
            ToolSearchMode::Native => {
                if !provider_has_native {
                    return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        format!(
                            "tool_search: provider \"{provider}\" does not expose native \
                         tool-search for model \"{model}\". Set \
                         `tool_search: {{ mode: \"client\" }}` to use the client-executed \
                         fallback, or omit tool_search to ship tools eagerly."
                        ),
                    ))));
                }
                ToolSearchResolution::Native
            }
            ToolSearchMode::Client => ToolSearchResolution::Client,
            ToolSearchMode::Auto => {
                if cfg.variant == ToolSearchVariant::Hybrid {
                    ToolSearchResolution::Client
                } else if provider_has_native {
                    ToolSearchResolution::Native
                } else {
                    ToolSearchResolution::Client
                }
            }
        };

        // Pre-flight (applies to both native and client): all-deferred
        // tool lists leave the model with no starting point. Anthropic
        // returns HTTP 400 on this and we match the diagnostic for
        // consistency across modes.
        if let Some(tools) = native_tools.as_ref() {
            let deferred = extract_deferred_tool_names(tools);
            let total_user_tools = tools.len();
            if total_user_tools > 0 && deferred.len() == total_user_tools {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "tool_search: all tools have defer_loading set. At least \
                     one tool must be non-deferred so the model has somewhere \
                     to start. (Matches Anthropic's 400 on the same condition.)",
                ))));
            }
        }

        match resolution {
            ToolSearchResolution::Native => {
                // Classify the native wire shape for this provider so
                // the injection and response parser agree on what to
                // emit / look for. Anthropic path emits the
                // `tool_search_tool_*_20251119` meta-tool; OpenAI path
                // emits `{"type": "tool_search"}`. For the "mock"
                // provider we infer from the model string so
                // conformance tests can exercise both paths without
                // HTTP. See `provider_native_tool_search_shape`.
                let shape = classify_native_shape(&capability_provider, &capability_model);
                match shape {
                    crate::llm::provider::NativeToolSearchShape::Anthropic => {
                        // Anthropic exposes {bm25, regex}. Variant
                        // names are documented in
                        // `effective_variants`; fall back to element 0
                        // with a warn if the user asked for something
                        // this model doesn't support.
                        if !variant_supported(cfg.variant.as_short()) {
                            crate::events::log_warn(
                                "llm.tool_search",
                                &format!(
                                    "provider \"{provider}\" model \"{model}\" does not support \
                                     tool_search variant \"{}\"; falling back to \"{}\"",
                                    cfg.variant.as_short(),
                                    effective_variants[0],
                                ),
                            );
                        }
                        let effective_variant = if variant_supported(cfg.variant.as_short()) {
                            cfg.variant
                        } else {
                            match effective_variants[0].as_str() {
                                "regex" => ToolSearchVariant::Regex,
                                _ => ToolSearchVariant::Bm25,
                            }
                        };
                        crate::llm::tools::apply_tool_search_native_injection_typed(
                            &mut native_tools,
                            shape,
                            effective_variant.as_short(),
                            "hosted",
                        );
                    }
                    crate::llm::provider::NativeToolSearchShape::OpenAi => {
                        // OpenAI Responses API exposes hosted + client
                        // modes. When the user picked `mode: "native"`
                        // they meant "let OpenAI handle the search on
                        // their side" — the hosted mode. Users who want
                        // Harn to execute the search locally should
                        // write `mode: "client"` for the stdlib agent
                        // loop fallback.
                        crate::llm::tools::apply_tool_search_native_injection_typed(
                            &mut native_tools,
                            shape,
                            cfg.variant.as_short(),
                            "hosted",
                        );
                        // OpenAI tool search is a Responses-only tool. The mock
                        // route follows the same projection contract so Harn
                        // conformance can verify endpoint selection offline.
                        if provider == "openai" || provider == "mock" {
                            api_mode = LlmApiMode::Responses;
                        }
                    }
                }
            }
            ToolSearchResolution::Client => {}
        }
    }

    let tool_choice = options
        .as_ref()
        .and_then(|o| o.get("tool_choice"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(vm_value_to_json);
    // tool_choice is accepted for any route that can call tools at all —
    // native or text-format. Text-format routes don't have a protocol-level
    // tool_choice field, but the value is still meaningful (e.g. `"none"`
    // signals "skip tool calls this turn") and providers like Ollama
    // forward it through. Gating only on `native_tools` blocked scripts
    // that legitimately request tool_choice on text-tool routes such as
    // `ollama/devstral-small-2:24b`.
    if enforce_capability_gates
        && tool_choice.is_some()
        && !caps.native_tools
        && !caps.text_tool_wire_format_supported
    {
        return Err(unsupported_option_error(
            "tool_choice",
            &capability_provider,
            &capability_model,
        ));
    }
    if parallel_tool_calls.is_some()
        && native_tools.as_ref().is_none_or(Vec::is_empty)
        && provider_tools.is_empty()
    {
        return Err(generation_option_error(
            "parallel_tool_calls",
            "requires at least one provider-native tool",
        ));
    }

    let selected_provider_overrides = options
        .as_ref()
        .and_then(|o| o.get("provider_options"))
        .and_then(|v| v.as_dict())
        .and_then(|namespaced| namespaced.get(provider.as_str()))
        .and_then(|v| v.as_dict());
    if let Some(overrides) = selected_provider_overrides {
        if let Some(path) = first_class_generation_wire_path(overrides) {
            return Err(generation_option_error(
                "provider_options",
                format!(
                    "`{path}` is owned by Harn's typed generation options and cannot bypass validation under `provider_options.{provider}`"
                ),
            ));
        }
    }
    let provider_overrides = selected_provider_overrides.map(vm_value_dict_to_json);
    let previous_response_id =
        opt_str(&options, "previous_response_id").filter(|value| !value.trim().is_empty());
    let store = opt_responses_store_field(options.as_ref())?;
    let data_controls = opt_data_posture_field(options.as_ref())?;
    let background = opt_bool_field(options.as_ref(), "background")?;
    let truncation = opt_str(&options, "truncation").filter(|value| !value.trim().is_empty());
    let compact = opt_bool_field(options.as_ref(), "compact")?;
    let include = opt_str_list(&options, "include");
    let max_tool_calls = opt_int(&options, "max_tool_calls");

    // Provider-side conversation state is not an OpenAI-Responses exclusive:
    // Gemini's Interactions endpoint family serves the same three knobs
    // (`previous_interaction_id` / `store` / `background`). Gate them on what
    // the route can actually do rather than on one provider's api_mode, so the
    // options stay one neutral vocabulary instead of two parallel spellings.
    // The remaining four have no Interactions representation and would be
    // silently dropped, so they still require Responses.
    if enforce_capability_gates && api_mode != LlmApiMode::Responses {
        let stateful_route = caps
            .live_endpoint_family
            .is_some_and(crate::llm::capabilities::LiveEndpointFamily::is_stateful);
        if !stateful_route
            && (previous_response_id.is_some() || store.is_some() || background.is_some())
        {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "previous_response_id / store / background require api_mode: \"responses\" or a route with provider-side conversation state",
            ))));
        }
        if truncation.is_some()
            || compact.is_some()
            || include.is_some()
            || max_tool_calls.is_some()
        {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "Responses-only options require api_mode: \"responses\"",
            ))));
        }
    }

    let prefill = options
        .as_ref()
        .and_then(|o| o.get("prefill"))
        .and_then(|v| {
            if matches!(v, VmValue::Nil) {
                None
            } else {
                let s = v.display();
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            }
        });
    let structural_experiment =
        crate::llm::structural_experiments::parse_structural_experiment_option(options.as_ref())?;
    let budget = crate::llm::cost::parse_budget(options.as_ref())?;
    let reminders = options
        .as_ref()
        .and_then(|o| o.get("reminders"))
        .map(vm_value_to_json);

    // `speed` is the serving-tier intent: "standard" (default) or "fast"
    // (the model's catalog-declared accelerated tier; "flex"/"batch" arrive
    // with the batch plane). The catalog is the source of truth for the
    // per-provider knob; the provider body builder reads
    // `serving_tiers[].request`.
    let fast = match opt_str(&options, "speed").as_deref() {
        None | Some("standard") => false,
        Some("fast") => true,
        Some(other) => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("llm_call: `speed` expects \"standard\" or \"fast\", got \"{other}\""),
            ))));
        }
    };
    if fast && enforce_capability_gates {
        match crate::llm::serving_tiers::fast_gate(&model) {
            crate::llm::serving_tiers::ServingTierGate::Usable => {}
            crate::llm::serving_tiers::ServingTierGate::Unsupported => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                    "fast: model \"{model}\" (provider \"{provider}\") has no accelerated-serving \
                     tier in the catalog; remove `fast` or pick a model that advertises a `fast` serving_tier"
                ),
                ))));
            }
            crate::llm::serving_tiers::ServingTierGate::Deprecated { note } => {
                let detail = note.map(|n| format!(" ({n})")).unwrap_or_default();
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                    "fast: the accelerated-serving tier for model \"{model}\" is deprecated{detail}"
                ),
                ))));
            }
        }
    }

    let mut opts = LlmCallOptions {
        provider,
        model,
        api_key: String::new(),
        api_mode,
        route_policy,
        fallback_chain,
        route_fallbacks,
        routing_decision,
        routing_policy,
        region: None,
        session_id,
        call_stage,
        rate_limit_consumer_id,
        rate_limit_reroute_on_timeout: false,
        mock_scope,
        done_sentinel: opt_str(&options, "_done_sentinel")
            .or_else(|| opt_str(&options, "done_sentinel"))
            .filter(|value| !value.trim().is_empty()),
        done_sentinel_form: opt_str(&options, "_done_sentinel_form")
            .filter(|value| !value.trim().is_empty()),
        dispatch_provenance,
        reminders,
        reminder_lifecycle,
        message_lineage,
        messages,
        system,
        system_prompt_root,
        context_manifest,
        transcript_summary: None,
        max_tokens,
        temperature,
        top_p,
        top_k,
        logprobs,
        logit_bias,
        min_p,
        repetition_penalty,
        prediction,
        verbosity,
        mirostat,
        stop,
        seed,
        frequency_penalty,
        presence_penalty,
        parallel_tool_calls,
        portable_option_intent,
        fast,
        output_format,
        output_schema,
        output_validation,
        schema_stream_abort,
        thinking,
        anthropic_beta_features,
        vision,
        tools: tools_val,
        native_tools,
        provider_tools,
        tool_choice,
        tool_search,
        cache,
        prompt_cache_ttl,
        timeout,
        idle_timeout,
        stream,
        provider_overrides,
        previous_response_id,
        data_controls,
        store,
        background,
        truncation,
        compact,
        include,
        max_tool_calls,
        budget,
        prefill,
        structural_experiment,
        applied_structural_experiment: None,
    };
    let equivalent_failover_policy = parse_equivalent_failover_option(
        options.as_ref(),
        &opts.provider,
        &opts.model,
        explicit_routing_policy,
        equivalent_failover_requirements_for_options(&opts),
    )?;
    if opts.routing_policy.is_none() {
        opts.routing_policy = equivalent_failover_policy.or_else(|| {
            crate::llm::routing::build_transport_failover_policy(
                &opts.provider,
                &opts.model,
                &opts.route_fallbacks,
                &opts.fallback_chain,
            )
        });
    }

    if enforce_capability_gates {
        validate_options(&opts)?;
    }
    if opts
        .routing_policy
        .as_ref()
        .is_none_or(|policy| policy.chain.len() <= 1)
        && opts.route_fallbacks.is_empty()
        && opts.fallback_chain.is_empty()
    {
        opts.api_key = resolve_api_key_for_selection(&opts.provider, selection_source)?;
    }
    Ok(opts)
}

/// True when the caller explicitly set `key` to a non-nil value in the raw
/// (pre-default-injection) options dict. Used to detect a user-written
/// `model:`/`provider:` that conflicts with a `models:`/`ladder:` option.
fn option_is_explicitly_set(options: &crate::value::DictMap, key: &str) -> bool {
    matches!(options.get(key), Some(value) if !matches!(value, VmValue::Nil))
}

fn parse_prompt_cache_ttl_option(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<crate::llm::api::PromptCacheTtl>, VmError> {
    let Some(value) = options.and_then(|o| o.get("prompt_cache_ttl")) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::String(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            crate::llm::api::PromptCacheTtl::parse(&normalized)
                .map(Some)
                .ok_or_else(|| {
                    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                        "llm_call: `prompt_cache_ttl` must be \"5m\" or \"1h\"",
                    )))
                })
        }
        other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
                "llm_call: `prompt_cache_ttl` must be a string, got {}",
                other.type_name()
            ),
        )))),
    }
}

pub(crate) fn opt_str_list(
    options: &Option<crate::value::DictMap>,
    key: &str,
) -> Option<Vec<String>> {
    let val = options.as_ref()?.get(key)?;
    match val {
        VmValue::List(list) => {
            let strs: Vec<String> = list.iter().map(|v| v.display()).collect();
            if strs.is_empty() {
                None
            } else {
                Some(strs)
            }
        }
        _ => None,
    }
}

/// Provider stop-sequence count cap. OpenAI and Anthropic both reject more than
/// four stop sequences, so a merged list is bounded to the tightest common
/// limit. The tool-call terminators lead the list, so enabling the text-lane
/// stop always keeps them within the cap even when the caller already supplied
/// stops.
const MAX_STOP_SEQUENCES: usize = 4;

/// Resolve the final `stop` list for one LLM call, injecting the text tool-call
/// terminator(s) for the text tool-call lane (harn#4743).
///
/// The injection is opt-in per call (`stop_at_tool_call`, armed by the host for
/// a measured rollout) and only applies when the route honors stop sequences
/// (`stop_supported` — a route that 400s on stop extras like xAI/Grok is never
/// sent one), the call actually carries tools, and the resolved lane is a text
/// channel (`text`/`json`). Otherwise the caller-supplied `stop` passes through
/// unchanged.
fn resolve_stop_sequences(
    options: &Option<crate::value::DictMap>,
    caller_stop: Option<Vec<String>>,
    tool_format: &str,
    stop_supported: bool,
    has_tools: bool,
) -> Option<Vec<String>> {
    let opted_in = opt_bool(options, "stop_at_tool_call");
    let text_lane = crate::llm_config::tool_format_channel(tool_format)
        == Some(crate::llm_config::ToolFormatChannel::Text);
    if opted_in && stop_supported && has_tools && text_lane {
        Some(merge_stop_with_tool_terminators(caller_stop))
    } else {
        caller_stop
    }
}

/// Merge the text tool-call terminators into a caller-supplied `stop` list. The
/// terminators lead (they are the functional stop the feature guarantees),
/// caller entries follow, duplicates are dropped, and the result is capped at
/// [`MAX_STOP_SEQUENCES`]. Stop order is semantically irrelevant to every
/// provider (any match ends generation), so leading with the terminators is
/// safe.
fn merge_stop_with_tool_terminators(caller_stop: Option<Vec<String>>) -> Vec<String> {
    let mut merged: Vec<String> = crate::llm::tools::text_tool_call_tag_pairs()
        .iter()
        .map(|(_, close)| (*close).to_string())
        .collect();
    for entry in caller_stop.into_iter().flatten() {
        if !merged.contains(&entry) {
            merged.push(entry);
        }
    }
    merged.truncate(MAX_STOP_SEQUENCES);
    merged
}

#[cfg(test)]
mod stop_sequence_tests {
    use super::{merge_stop_with_tool_terminators, resolve_stop_sequences, MAX_STOP_SEQUENCES};
    use crate::llm::tools::{TEXT_TOOL_CALL_CLOSE, TEXT_TOOL_CALL_CLOSE_COMPACT};
    use crate::value::{DictMap, VmDictExt, VmValue};

    fn opts_flag(value: bool) -> Option<DictMap> {
        let mut dict = DictMap::new();
        dict.put("stop_at_tool_call", VmValue::Bool(value));
        Some(dict)
    }

    fn terminators() -> Vec<String> {
        vec![
            TEXT_TOOL_CALL_CLOSE.to_string(),
            TEXT_TOOL_CALL_CLOSE_COMPACT.to_string(),
        ]
    }

    #[test]
    fn merge_leads_with_terminators_and_dedupes() {
        assert_eq!(merge_stop_with_tool_terminators(None), terminators());
        // A caller stop that already names the primary terminator must not
        // duplicate it; distinct caller entries follow the terminators.
        let merged = merge_stop_with_tool_terminators(Some(vec![
            TEXT_TOOL_CALL_CLOSE.to_string(),
            "STOP".to_string(),
        ]));
        assert_eq!(
            merged,
            vec![
                TEXT_TOOL_CALL_CLOSE.to_string(),
                TEXT_TOOL_CALL_CLOSE_COMPACT.to_string(),
                "STOP".to_string(),
            ]
        );
    }

    #[test]
    fn merge_caps_at_provider_limit_keeping_terminators() {
        let merged = merge_stop_with_tool_terminators(Some(vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ]));
        assert_eq!(merged.len(), MAX_STOP_SEQUENCES);
        // The functional terminators survive the cap; caller overflow is dropped.
        assert_eq!(merged[0], TEXT_TOOL_CALL_CLOSE);
        assert_eq!(merged[1], TEXT_TOOL_CALL_CLOSE_COMPACT);
    }

    #[test]
    fn injects_terminators_for_text_lane_when_opted_in() {
        assert_eq!(
            resolve_stop_sequences(&opts_flag(true), None, "text", true, true),
            Some(terminators()),
        );
        // `json` is also a text-channel format.
        assert_eq!(
            resolve_stop_sequences(&opts_flag(true), None, "json", true, true),
            Some(terminators()),
        );
    }

    #[test]
    fn passes_caller_stop_through_when_not_opted_in() {
        let caller = Some(vec!["DONE".to_string()]);
        assert_eq!(
            resolve_stop_sequences(&opts_flag(false), caller.clone(), "text", true, true),
            caller,
        );
        assert_eq!(
            resolve_stop_sequences(&None, None, "text", true, true),
            None,
        );
    }

    #[test]
    fn does_not_inject_outside_the_gated_conditions() {
        // Native lane: the terminator is not part of the wire, so never inject.
        assert_eq!(
            resolve_stop_sequences(&opts_flag(true), None, "native", true, true),
            None,
        );
        // Route rejects stop extras (e.g. xAI/Grok): never inject.
        assert_eq!(
            resolve_stop_sequences(&opts_flag(true), None, "text", false, true),
            None,
        );
        // No tools on the call: nothing to terminate.
        assert_eq!(
            resolve_stop_sequences(&opts_flag(true), None, "text", true, false),
            None,
        );
    }
}

#[cfg(test)]
mod cache_default_tests {
    use super::extract_llm_options;
    use crate::llm::api::PromptCacheTtl;
    use crate::value::{DictMap, VmDictExt, VmError, VmValue};

    fn opts_with(options: DictMap) -> crate::llm::api::LlmCallOptions {
        extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello")),
            VmValue::Nil,
            VmValue::dict(options),
        ])
        .expect("options")
    }

    fn try_opts_with(options: DictMap) -> Result<crate::llm::api::LlmCallOptions, VmError> {
        extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello")),
            VmValue::Nil,
            VmValue::dict(options),
        ])
    }

    fn thrown_message(err: VmError) -> String {
        match err {
            VmError::Thrown(VmValue::String(message)) => message.to_string(),
            VmError::Thrown(VmValue::Dict(fields)) => fields
                .get("message")
                .map(VmValue::display)
                .unwrap_or_else(|| "missing structured error message".to_string()),
            other => format!("{other:?}"),
        }
    }

    // Install an authored mock route so admission sees both prompt caching and
    // the provider-specific TTL lowering without requiring a live API key. A
    // bare mock model still falls through to non-caching defaults.
    fn caching_route() -> DictMap {
        crate::llm::capabilities::set_user_overrides_toml(
            r#"
[[provider.mock]]
model_match = "claude-sonnet-4.6"
prompt_caching = true
prompt_cache_ttls = ["5m", "1h"]
"#,
        )
        .expect("mock cache capability override");
        let mut options = DictMap::new();
        options.put_str("provider", "mock");
        options.put_str("model", "claude-sonnet-4.6");
        options
    }

    fn non_caching_route() -> DictMap {
        let mut options = DictMap::new();
        options.put_str("provider", "mock");
        options.put_str("model", "no-cache-model");
        options
    }

    #[test]
    fn rate_limit_consumer_identity_defaults_to_session_and_can_be_overridden() {
        let mut options = non_caching_route();
        options.put_str("session_id", "session-a");
        let session_scoped = opts_with(options);
        assert_eq!(
            session_scoped.rate_limit_consumer_id.as_deref(),
            Some("session-a")
        );

        let mut options = non_caching_route();
        options.put_str("session_id", "session-b");
        options.put_str("rate_limit_consumer_id", "tenant-7");
        let tenant_scoped = opts_with(options);
        assert_eq!(
            tenant_scoped.rate_limit_consumer_id.as_deref(),
            Some("tenant-7")
        );
    }

    #[test]
    fn semantic_call_role_is_shared_with_fixture_scope_and_never_inferred() {
        let unattributed = opts_with(non_caching_route());
        assert_eq!(
            unattributed.context_manifest.call_role(),
            "unattributed",
            "absence must stay observable instead of masquerading as a valid purpose"
        );
        assert_eq!(unattributed.mock_scope, None);

        let mut router_options = non_caching_route();
        router_options.put_str("call_role", "model.router");
        let router = opts_with(router_options);
        assert_eq!(router.context_manifest.call_role(), "model.router");
        assert_eq!(router.mock_scope.as_deref(), Some("model.router"));

        let mut agent_options = non_caching_route();
        agent_options.put_str("mock_scope", "agent.main");
        let agent = opts_with(agent_options);
        assert_eq!(agent.context_manifest.call_role(), "agent.main");

        let mut judge_options = non_caching_route();
        judge_options.put_str("mock_scope", "completion.judge");
        let judge = opts_with(judge_options);
        assert_eq!(judge.context_manifest.call_role(), "completion.judge");

        let roles = [
            router.context_manifest.call_role(),
            agent.context_manifest.call_role(),
            judge.context_manifest.call_role(),
        ];
        assert_eq!(
            roles
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3,
            "router, agent-under-test, and completion judge must be distinguishable by role alone"
        );
    }

    #[test]
    fn call_role_and_fixture_scope_must_not_disagree() {
        let mut options = non_caching_route();
        options.put_str("call_role", "model.router");
        options.put_str("mock_scope", "agent.main");
        let error = try_opts_with(options).expect_err("semantic purpose mismatch must fail");
        assert!(
            thrown_message(error).contains("call_role `model.router` disagrees"),
            "unexpected error"
        );
    }

    #[test]
    fn cache_defaults_off_for_non_supporting_route() {
        // A route whose capability matrix says `prompt_caching = false` must
        // resolve the default to OFF so the outgoing request stays byte-
        // identical and the strict capability gate never trips on a default.
        assert!(
            !crate::llm::capabilities::lookup("mock", "no-cache-model").prompt_caching,
            "precondition: route must not support caching"
        );
        assert!(
            !opts_with(non_caching_route()).cache,
            "cache default must be OFF when the route does not support caching"
        );
    }

    #[test]
    fn cache_defaults_on_for_supporting_route() {
        // When the route supports prompt caching, the stable system+tools+
        // history prefix is marked cacheable by default so multi-turn loops
        // and the rubric grader pay the discounted cached-input rate.
        assert!(
            crate::llm::capabilities::lookup("mock", "claude-sonnet-4.6").prompt_caching,
            "precondition: route must support caching"
        );
        assert!(
            opts_with(caching_route()).cache,
            "cache must default ON for a caching-capable route"
        );
    }

    #[test]
    fn explicit_cache_false_opts_out_on_supporting_route() {
        let mut options = caching_route();
        options.put("cache", VmValue::Bool(false));
        assert!(
            !opts_with(options).cache,
            "explicit `cache: false` must opt out"
        );
    }

    #[test]
    fn models_ladder_lowers_to_routing_policy() {
        let mut options = DictMap::new();
        options.put(
            "models",
            VmValue::List(std::sync::Arc::new(vec![
                VmValue::String(arcstr::ArcStr::from("mock-cheap")),
                VmValue::String(arcstr::ArcStr::from("mock-strong")),
            ])),
        );
        let opts = opts_with(options);
        let policy = opts
            .routing_policy
            .expect("ladder lowered to routing policy");
        assert!(policy.is_ladder);
        assert_eq!(policy.chain.len(), 2);
        // Base provider/model snap to the first rung.
        assert_eq!(opts.model, "mock-cheap");
    }

    #[test]
    fn models_ladder_conflicts_with_explicit_model() {
        let mut options = DictMap::new();
        options.put(
            "models",
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("mock-cheap"),
            )])),
        );
        options.put_str("model", "pinned-model");
        let result = try_opts_with(options);
        let err = match result {
            Ok(_) => panic!("models + model must be rejected as ambiguous"),
            Err(err) => err,
        };
        assert!(format!("{err:?}").contains("cannot be combined"));
    }

    #[test]
    fn explicit_cache_true_errors_on_non_supporting_route() {
        // The strict capability gate is preserved: an explicit `cache: true`
        // on a route that cannot cache surfaces a loud error rather than a
        // silent no-op (unchanged behavior).
        let mut options = non_caching_route();
        options.put("cache", VmValue::Bool(true));
        assert!(
            try_opts_with(options).is_err(),
            "explicit `cache: true` on a non-supporting route must error"
        );
    }

    #[test]
    fn prompt_cache_ttl_one_hour_parses_for_anthropic_route() {
        let mut options = caching_route();
        options.put_str("prompt_cache_ttl", "1h");
        let opts = opts_with(options);
        assert_eq!(opts.prompt_cache_ttl, Some(PromptCacheTtl::OneHour));
        assert!(opts.cache, "TTL requests keep provider prompt caching on");
    }

    #[test]
    fn prompt_cache_ttl_rejects_invalid_values() {
        let mut options = caching_route();
        options.put_str("prompt_cache_ttl", "24h");
        let err = match try_opts_with(options) {
            Ok(_) => panic!("invalid TTL must error"),
            Err(err) => err,
        };
        assert!(thrown_message(err).contains("must be \"5m\" or \"1h\""));
    }

    #[test]
    fn prompt_cache_ttl_conflicts_with_cache_false() {
        let mut options = caching_route();
        options.put("cache", VmValue::Bool(false));
        options.put_str("prompt_cache_ttl", "1h");
        let err = match try_opts_with(options) {
            Ok(_) => panic!("cache false + TTL must error"),
            Err(err) => err,
        };
        assert!(thrown_message(err).contains("requires provider prompt caching"));
    }

    #[test]
    fn prompt_cache_ttl_errors_on_non_supporting_route() {
        let mut options = non_caching_route();
        options.put_str("prompt_cache_ttl", "1h");
        let err = match try_opts_with(options) {
            Ok(_) => panic!("unsupported TTL must error"),
            Err(err) => err,
        };
        assert!(thrown_message(err).contains("prompt_cache_ttl"));
    }
}
