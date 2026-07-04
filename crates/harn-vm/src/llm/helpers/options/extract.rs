use super::*;
use super::{
    defaults::*, json::*, output::*, reminders::*, routing::*, system_prompt::*, thinking::*,
    tool_search::*,
};

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
    // Capture whether the CALLER (not injected defaults) pinned a model /
    // provider before `explicit_options` is consumed by the context merge —
    // needed to reject `models:`/`ladder:` combined with a standalone route.
    let user_pinned_route = explicit_options.as_ref().is_some_and(|raw| {
        option_is_explicitly_set(raw, "model") || option_is_explicitly_set(raw, "provider")
    });
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
    // burin-code). The `models:`/`ladder:` + explicit `routing:` combination is
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
    let equivalent_failover_policy = parse_equivalent_failover_option(
        options.as_ref(),
        &provider,
        &model,
        explicit_routing_policy,
    )?;
    if routing_policy.is_none() {
        routing_policy = equivalent_failover_policy;
    }
    // A routing_policy chain owns provider/model selection: snap the
    // base options to the first link so transcript-only consumers see
    // a sensible placeholder. The executor swaps these per attempt.
    if let Some(policy) = routing_policy.as_ref() {
        if let Some(first) = policy.chain.first() {
            provider = first.provider.clone();
            model = first.model.clone();
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
    let api_key = resolve_api_key(&provider)?;
    let caps = crate::llm::capabilities::lookup(&provider, &model);
    let api_mode = parse_api_mode_option(options.as_ref())?;
    if enforce_responses_provider_gate(api_mode, &provider) {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "api_mode: \"responses\" is only supported by provider \"openai\"; got provider \"{provider}\""
        )))));
    }
    let session_id = opt_str(&options, "session_id")
        .filter(|value| !value.is_empty())
        .or_else(crate::agent_sessions::current_session_id);
    // Provenance annotation supplied by the pipeline resolver (which resolved
    // field came from a pin vs. was inherited from the primary). Observability
    // only — emitted verbatim into the `resolved_dispatch` transcript record.
    let dispatch_provenance = options
        .as_ref()
        .and_then(|o| o.get("dispatch_provenance"))
        .and_then(crate::llm::resolved_dispatch::DispatchProvenance::from_vm_value);
    let pending_reminders = pending_reminders_from_session(session_id.as_deref());
    let rendered_reminders = render_pending_reminders(&caps, &pending_reminders);
    let reminder_lifecycle = rendered_reminder_lifecycle(
        session_id.as_deref(),
        opt_int(&options, "_iteration").unwrap_or(0),
        &pending_reminders,
        &rendered_reminders,
    );
    let system =
        compose_system_prompt_with_reminders(system, options.as_ref(), &rendered_reminders)?;
    let enforce_capability_gates = !crate::llm::mock::cli_llm_mock_replay_active()
        && !crate::llm::mock::builtin_llm_mock_active();

    // Apply providers.toml model_defaults as fallbacks for unspecified params
    // (e.g. presence_penalty=1.5 for Qwen to avoid repetition loops).
    let model_defaults = crate::llm_config::model_params(&model);
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(|v| v.as_float()) };
    let default_int =
        |key: &str| -> Option<i64> { model_defaults.get(key).and_then(|v| v.as_integer()) };

    let max_tokens = opt_int(&options, "max_tokens").unwrap_or(16384);
    let temperature = opt_float(&options, "temperature").or_else(|| default_float("temperature"));
    let top_p = opt_float(&options, "top_p").or_else(|| default_float("top_p"));
    let top_k = opt_int(&options, "top_k").or_else(|| default_int("top_k"));
    let logprobs = opt_bool(&options, "logprobs");
    let top_logprobs = opt_int(&options, "top_logprobs");
    let stop = opt_str_list(&options, "stop");
    let seed = opt_int(&options, "seed");
    let frequency_penalty =
        opt_float(&options, "frequency_penalty").or_else(|| default_float("frequency_penalty"));
    let presence_penalty =
        opt_float(&options, "presence_penalty").or_else(|| default_float("presence_penalty"));
    let timeout = resolve_timeout_secs(
        opt_int(&options, "timeout"),
        opt_int(&options, "timeout_ms"),
    );
    let idle_timeout = opt_int(&options, "idle_timeout").map(|t| t as u64);
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
    let stream = options
        .as_ref()
        .and_then(|o| o.get("stream"))
        .map(|v| v.is_truthy())
        .unwrap_or_else(|| {
            std::env::var("HARN_LLM_STREAM")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true)
        });
    let output_validation = opt_str(&options, "output_validation");

    let reasoning_policy_application = crate::llm::reasoning_policy::resolve_for_llm_call(
        options.as_ref(),
        &provider,
        &model,
        &caps,
    )?;
    let thinking_from_reasoning_policy = reasoning_policy_application.is_some();
    let policy_thinking = reasoning_policy_application
        .as_ref()
        .map(|application| application.thinking.clone());

    let reasoning_effort = parse_reasoning_effort_option(options.as_ref())?;
    let thinking_from_reasoning_effort = reasoning_effort.is_some()
        && !options
            .as_ref()
            .and_then(|o| o.get("thinking"))
            .is_some_and(|value| value.is_truthy());
    let thinking = if let Some(level) = reasoning_effort {
        if options
            .as_ref()
            .and_then(|o| o.get("thinking"))
            .is_some_and(|value| value.is_truthy())
        {
            return Err(thinking_error(
                "reasoning_effort cannot be combined with a non-disabled thinking option",
            ));
        }
        crate::llm::api::ThinkingConfig::Effort { level }
    } else if let Some(thinking) = policy_thinking {
        thinking
    } else {
        parse_thinking_option(options.as_ref())?
    };
    let reasoning_effort_requires_provider_support = matches!(
        thinking,
        crate::llm::api::ThinkingConfig::Effort { level }
            if level != crate::llm::api::ReasoningEffort::None
    );
    if enforce_capability_gates
        && thinking_from_reasoning_effort
        && reasoning_effort_requires_provider_support
        && !caps.reasoning_effort_supported
    {
        return Err(unsupported_option_error(
            "reasoning_effort",
            &provider,
            &model,
        ));
    }
    if enforce_capability_gates {
        validate_thinking_supported(
            &thinking,
            &provider,
            &model,
            &caps.thinking_modes,
            if thinking_from_reasoning_effort {
                "reasoning_effort"
            } else if thinking_from_reasoning_policy {
                "reasoning_policy"
            } else {
                "thinking"
            },
        )?;
        validate_reasoning_effort_level_supported(
            &thinking,
            &provider,
            &model,
            &caps,
            if thinking_from_reasoning_effort {
                "reasoning_effort"
            } else if thinking_from_reasoning_policy {
                "reasoning_policy"
            } else {
                "thinking"
            },
        )?;
    }
    let mut anthropic_beta_features = parse_anthropic_beta_features_option(
        options.as_ref(),
        &thinking,
        &provider,
        &model,
        enforce_capability_gates,
    )?;

    let response_format = opt_str(&options, "response_format");
    let json_schema = parse_schema_value(
        options
            .as_ref()
            .and_then(|o| o.get("json_schema").or_else(|| o.get("schema"))),
        "json_schema",
    )?;
    let output_schema = parse_schema_value(
        options.as_ref().and_then(|o| {
            o.get("output_schema")
                .or_else(|| o.get("json_schema"))
                .or_else(|| o.get("schema"))
        }),
        "output_schema",
    )?;
    let output_format = parse_output_format_option(
        options.as_ref(),
        response_format.as_deref(),
        json_schema.as_ref(),
    )?;
    if enforce_capability_gates {
        validate_output_format_supported(&output_format, &provider, &model, &caps)?;
    }
    let output_schema = output_schema.or_else(|| output_format.schema().cloned());
    // `schema_stream_abort` defaults to true whenever a schema is in play,
    // so callers that expect structured output get the early-abort win
    // automatically. Explicit `false` opts out and lets the stream run to
    // completion (relying on `schema_retries` for post-hoc recovery).
    let schema_stream_abort = match options.as_ref().and_then(|o| o.get("schema_stream_abort")) {
        Some(VmValue::Bool(value)) => *value,
        Some(VmValue::Nil) | None => output_schema.is_some(),
        Some(other) => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "llm_call: `schema_stream_abort` must be a bool, got {}",
                    other.type_name()
                ),
            ))));
        }
    };

    // Reject the deprecated `transcript` option key. Conversation
    // lifecycle is expressed through `session_id` + the explicit
    // `agent_session_*` builtins; there is no opaque transcript dict to
    // pass around anymore.
    if options.as_ref().and_then(|o| o.get("transcript")).is_some() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "llm_call / agent_loop: the `transcript` option was removed. \
                 Open or open-and-resume a session with agent_session_open(id) \
                 and pass `session_id: id` instead.",
        ))));
    }

    // Message source precedence: options.messages > prompt.
    let messages_val = options.as_ref().and_then(|o| o.get("messages")).cloned();
    let messages = if let Some(VmValue::List(msg_list)) = &messages_val {
        vm_messages_to_json(msg_list)?
    } else {
        vec![serde_json::json!({"role": "user", "content": prompt})]
    };
    let messages = apply_rendered_reminder_messages(messages, &rendered_reminders);
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
        return Err(unsupported_option_error("vision", &provider, &model));
    }
    if enforce_capability_gates && audio && !caps.audio {
        return Err(unsupported_option_error("audio", &provider, &model));
    }
    if enforce_capability_gates && pdf && !caps.pdf {
        return Err(unsupported_option_error("pdf", &provider, &model));
    }
    if enforce_capability_gates && video && !caps.video {
        return Err(unsupported_option_error("video", &provider, &model));
    }
    if enforce_capability_gates && uses_file_ids && !caps.files_api_supported {
        return Err(unsupported_option_error("files_api", &provider, &model));
    }
    if uses_file_ids && caps.message_wire_format.is_anthropic() {
        crate::llm::api::push_unique_anthropic_beta_feature(
            &mut anthropic_beta_features,
            crate::stdlib::files::ANTHROPIC_FILES_API_BETA,
        );
    }
    if enforce_capability_gates && cache && !caps.prompt_caching {
        return Err(unsupported_option_error("cache", &provider, &model));
    }
    if vision
        && !crate::llm::provider::provider_supports_image_urls(&provider, &model)
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
    let requested_tool_format = opt_str(&options, "tool_format")
        .unwrap_or_else(|| crate::llm_config::default_tool_format(&model, &provider));
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
    if enforce_capability_gates && tools_val.is_some() {
        if let Some(message) =
            crate::llm::capabilities::no_viable_tool_channel_with_caps(&provider, &model, &caps)
        {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                message,
            ))));
        }
    }
    let tool_format = if enforce_capability_gates && tools_val.is_some() {
        let decision = crate::llm::capabilities::validate_tool_format_with_caps(
            &provider,
            &model,
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
    {
        return Err(unsupported_option_error("tools", &provider, &model));
    }
    let mut native_tools = if tool_format == "native" {
        if let Some(tools) = &tools_val {
            Some(vm_tools_to_native(tools, &provider, &model)?)
        } else {
            None
        }
    } else {
        None
    };
    let provider_tools = parse_provider_tools_option(options.as_ref())?;
    if enforce_capability_gates && !provider_tools.is_empty() && api_mode != LlmApiMode::Responses {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "provider_tools requires api_mode: \"responses\"",
        ))));
    }

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
        let native_variants = provider_tool_search_variants(&provider, &model);
        let model_based_native =
            provider_supports_defer_loading(&provider, &model) && !native_variants.is_empty();
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
                let shape = classify_native_shape(&provider, &model);
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
        return Err(unsupported_option_error("tool_choice", &provider, &model));
    }

    let provider_overrides = options
        .as_ref()
        .and_then(|o| o.get(provider.as_str()))
        .and_then(|v| v.as_dict())
        .map(vm_value_dict_to_json);
    let previous_response_id =
        opt_str(&options, "previous_response_id").filter(|value| !value.trim().is_empty());
    let store = opt_responses_store_field(options.as_ref())?;
    let background = opt_bool_field(options.as_ref(), "background")?;
    let truncation = opt_str(&options, "truncation").filter(|value| !value.trim().is_empty());
    let compact = opt_bool_field(options.as_ref(), "compact")?;
    let include = opt_str_list(&options, "include");
    let max_tool_calls = opt_int(&options, "max_tool_calls");

    if enforce_capability_gates
        && api_mode != LlmApiMode::Responses
        && (previous_response_id.is_some()
            || store.is_some()
            || background.is_some()
            || truncation.is_some()
            || compact.is_some()
            || include.is_some()
            || max_tool_calls.is_some())
    {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "Responses-only options require api_mode: \"responses\"",
        ))));
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
    let budget = crate::llm::cost::parse_budget_envelope(options.as_ref())?;
    let reminders = options
        .as_ref()
        .and_then(|o| o.get("reminders"))
        .map(vm_value_to_json);

    // `fast: true` (or the provider-flavored `speed: "fast"`) opts into the
    // model's accelerated-serving tier. The catalog is the source of truth
    // for the per-provider knob, so we only validate the request is sane
    // here; the provider body builder reads `fast_mode.param`/`value`.
    let fast = opt_bool(&options, "fast") || opt_str(&options, "speed").as_deref() == Some("fast");
    if fast && enforce_capability_gates {
        match crate::llm::fast_mode::gate(&model) {
            crate::llm::fast_mode::FastModeGate::Usable => {}
            crate::llm::fast_mode::FastModeGate::Unsupported => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                    "fast: model \"{model}\" (provider \"{provider}\") has no accelerated-serving \
                     tier in the catalog; remove `fast` or pick a model that advertises `fast_mode`"
                ),
                ))));
            }
            crate::llm::fast_mode::FastModeGate::Deprecated { note } => {
                let detail = note.map(|n| format!(" ({n})")).unwrap_or_default();
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                    "fast: the accelerated-serving tier for model \"{model}\" is deprecated{detail}"
                ),
                ))));
            }
        }
    }

    let opts = LlmCallOptions {
        provider,
        model,
        api_key,
        api_mode,
        route_policy,
        fallback_chain,
        route_fallbacks,
        routing_decision,
        routing_policy,
        region: None,
        session_id,
        dispatch_provenance,
        reminders,
        reminder_lifecycle,
        messages,
        system,
        transcript_summary: None,
        max_tokens,
        temperature,
        top_p,
        top_k,
        logprobs,
        top_logprobs,
        stop,
        seed,
        frequency_penalty,
        presence_penalty,
        fast,
        output_format,
        response_format,
        json_schema,
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
        timeout,
        idle_timeout,
        stream,
        provider_overrides,
        previous_response_id,
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

    validate_options(&opts);
    Ok(opts)
}

/// True when the caller explicitly set `key` to a non-nil value in the raw
/// (pre-default-injection) options dict. Used to detect a user-written
/// `model:`/`provider:` that conflicts with a `models:`/`ladder:` option.
fn option_is_explicitly_set(options: &crate::value::DictMap, key: &str) -> bool {
    matches!(options.get(key), Some(value) if !matches!(value, VmValue::Nil))
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

thread_local! {
    /// Unsupported-param warnings already emitted this process, keyed by
    /// `"{param}|{provider}|{model}"`. `validate_options` runs on every LLM
    /// call, so without this the same "ignoring top_k" line floods agent
    /// logs once per turn. The capability mismatch is a static config fact,
    /// so one warning per (param, provider, model) is all the operator needs.
    static WARNED_UNSUPPORTED: std::cell::RefCell<std::collections::BTreeSet<String>> =
        const { std::cell::RefCell::new(std::collections::BTreeSet::new()) };
}

/// Returns true the first time a `(param, provider, model)` triple is seen,
/// false on every repeat. Lets `validate_options` warn once instead of on
/// every LLM call.
fn first_unsupported_warning(param: &str, provider: &str, model: &str) -> bool {
    let key = format!("{param}|{provider}|{model}");
    WARNED_UNSUPPORTED.with(|seen| seen.borrow_mut().insert(key))
}

/// Emit warnings for options not supported by the target provider.
pub(super) fn validate_options(opts: &crate::llm::api::LlmCallOptions) {
    let caps = crate::llm::capabilities::lookup(&opts.provider, &opts.model);
    let warn = |param: &str| {
        if !first_unsupported_warning(param, &opts.provider, &opts.model) {
            return;
        }
        crate::events::log_warn(
            "llm",
            &format!(
                "\"{param}\" is not supported by provider \"{}\" model \"{}\", ignoring",
                opts.provider, opts.model
            ),
        );
    };

    if opts.seed.is_some() && !caps.seed_supported {
        warn("seed");
    }
    if opts.top_k.is_some() && !caps.top_k_supported {
        warn("top_k");
    }
    if opts.temperature.is_some() && !caps.temperature_supported {
        warn("temperature");
    }
    if opts.top_p.is_some() && !caps.top_p_supported {
        warn("top_p");
    }
    if opts.frequency_penalty.is_some() && !caps.frequency_penalty_supported {
        warn("frequency_penalty");
    }
    if opts.presence_penalty.is_some() && !caps.presence_penalty_supported {
        warn("presence_penalty");
    }
    if opts.cache && !caps.prompt_caching {
        warn("cache");
    }
}

#[cfg(test)]
mod cache_default_tests {
    use super::extract_llm_options;
    use crate::value::{DictMap, VmDictExt, VmValue};

    fn opts_with(options: DictMap) -> crate::llm::api::LlmCallOptions {
        extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello")),
            VmValue::Nil,
            VmValue::dict(options),
        ])
        .expect("options")
    }

    fn try_opts_with(options: DictMap) -> Result<crate::llm::api::LlmCallOptions, super::VmError> {
        extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello")),
            VmValue::Nil,
            VmValue::dict(options),
        ])
    }

    // The `mock` provider needs no API key and resolves its capability row by
    // spoofing the real provider family of the model-id shape: a Claude-shaped
    // id lands on the Anthropic row (prompt_caching = true), while a bare,
    // unrecognised id falls through to defaults (prompt_caching = false). This
    // lets us exercise both default branches against the real capability
    // lookup, offline.
    fn caching_route() -> DictMap {
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
}

#[cfg(test)]
mod unsupported_warning_tests {
    use super::first_unsupported_warning;

    #[test]
    fn warns_once_per_param_provider_model() {
        // Use a unique synthetic triple so the process-wide thread-local set
        // is not polluted by (or polluting) any real provider calls.
        let m = "test-model-warns-once";
        assert!(
            first_unsupported_warning("top_k", "openrouter", m),
            "first sighting must warn"
        );
        assert!(
            !first_unsupported_warning("top_k", "openrouter", m),
            "repeat must be silent"
        );
        assert!(
            !first_unsupported_warning("top_k", "openrouter", m),
            "still silent on third call"
        );
    }

    #[test]
    fn distinct_param_provider_model_each_warn_once() {
        let m = "test-model-distinct";
        // Different param, different provider, and different model are each a
        // distinct key that warns on its own first sighting.
        assert!(first_unsupported_warning("top_k", "openrouter", m));
        assert!(first_unsupported_warning("seed", "openrouter", m));
        assert!(first_unsupported_warning("top_k", "other-prov", m));
        assert!(first_unsupported_warning(
            "top_k",
            "openrouter",
            "test-model-distinct-2"
        ));
        // ...and each repeat stays silent.
        assert!(!first_unsupported_warning("top_k", "openrouter", m));
        assert!(!first_unsupported_warning("seed", "openrouter", m));
    }
}
