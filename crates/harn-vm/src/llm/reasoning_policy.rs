//! Provider-aware reasoning policy normalization.
//!
//! `thinking` and `reasoning_effort` are intentionally lower-level: each
//! provider exposes a different wire shape. This module owns Harn's
//! user-facing policy vocabulary (`auto`, `off`, `low`, ...), lowering it
//! into the canonical [`ThinkingConfig`] that providers already understand.

use crate::llm::api::{ReasoningEffort, ThinkingConfig};
use crate::llm::capabilities::Capabilities;
use crate::value::{VmError, VmValue};

pub const INHERIT_POLICY_VALUE: &str = "@inherit";

const POLICY_VALUES: &[&str] = &["auto", "off", "minimal", "low", "medium", "high", "xhigh"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReasoningPolicyApplication {
    pub thinking: ThinkingConfig,
    pub policy: String,
    pub task: String,
    pub scale: String,
    pub level: String,
    pub provider: String,
    pub model: String,
}

/// Normalize a selector from ACP or another string-only host surface.
///
/// `None` means "clear the session pin". `"none"` is accepted as a
/// user-friendly alias for Harn's provider-agnostic `"off"` policy; raw
/// OpenAI `reasoning_effort: "none"` remains available through the lower-level
/// per-call option.
pub fn normalize_policy_selector(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == INHERIT_POLICY_VALUE {
        return Ok(None);
    }
    normalize_policy_str(trimmed).map(Some)
}

pub fn policy_values() -> &'static [&'static str] {
    POLICY_VALUES
}

pub(crate) fn resolve_for_llm_call(
    options: Option<&crate::value::DictMap>,
    provider: &str,
    model: &str,
    caps: &Capabilities,
) -> Result<Option<ReasoningPolicyApplication>, VmError> {
    resolve_policy(options, provider, model, caps, None)
}

pub(crate) fn apply_policy_to_vm_options(
    opts: &crate::value::DictMap,
) -> Result<crate::value::DictMap, VmError> {
    if caller_set_reasoning(Some(opts)) {
        return Ok(opts.clone());
    }
    let Some((provider, model)) = resolved_route_from_options(opts) else {
        return Ok(opts.clone());
    };
    let caps = crate::llm::capabilities::lookup(&provider, &model);
    let Some(application) = resolve_policy(Some(opts), &provider, &model, &caps, Some("auto"))?
    else {
        return Ok(opts.clone());
    };
    let mut out = opts.clone();
    out.insert(
        crate::value::intern_key("thinking"),
        thinking_to_vm_value(&application.thinking),
    );
    out.insert(
        crate::value::intern_key("_agent_reasoning_policy_applied"),
        application_metadata_to_vm_value(&application),
    );
    Ok(out)
}

fn resolve_policy(
    options: Option<&crate::value::DictMap>,
    provider: &str,
    model: &str,
    caps: &Capabilities,
    default_policy: Option<&str>,
) -> Result<Option<ReasoningPolicyApplication>, VmError> {
    if caller_set_reasoning(options) {
        return Ok(None);
    }
    let Some(policy) = selected_policy(options, default_policy)? else {
        return Ok(None);
    };
    let task = reasoning_task(options)?;
    let scale = reasoning_scale(options)?;
    let mut level = if policy == "auto" {
        auto_reasoning_level(&task, &scale)
    } else {
        policy.clone()
    };
    // Per-route auto-policy overrides live in the capability matrix
    // (`auto_reasoning_overrides` in capabilities.toml) so per-model
    // known regressions are declared alongside that model's other wire
    // capabilities, not as hard-coded provider/model patterns here.
    //
    // Two opposite tool-calling reasoning quirks are declared in the
    // matrix, not branched on here:
    //
    //   * Qwen3 (reasoning-OFF-for-tools): thinking mode plus tool calls
    //     is a binary on/off regression in the model weights — the model
    //     narrates its tool-call intent inside the reasoning trace and
    //     emits zero structured `tool_calls`. Qwen's own guidance is to
    //     disable reasoning when tool-calling. See
    //     https://github.com/QwenLM/Qwen3.6/issues/89. Routes that need
    //     the downgrade declare `auto_reasoning_overrides = { agent =
    //     "off" }`.
    //
    //   * gpt-oss / Harmony (reasoning-ON-for-tools): the model performs
    //     tool calls *inside* the Harmony chain-of-thought channel, so
    //     disabling reasoning yields 0 tool_calls and a tiny
    //     billed-noncommittal completion. Live OpenRouter probe of
    //     openai/gpt-oss-120b: reasoning disabled -> 0/N tool calls;
    //     reasoning low/default -> clean native tool calls. Such routes
    //     declare `reasoning_required_for_tools = true` and MUST NOT carry
    //     the Qwen-style `agent/verify/code = "off"` override.
    //
    // Callers that explicitly set a non-auto policy keep their override.
    if policy == "auto" {
        if let Some(override_level) = caps.auto_reasoning_overrides.get(&task) {
            level = override_level.clone();
        }
    }
    // Defense-in-depth for the gpt-oss / Harmony quirk: if this route calls
    // tools inside its reasoning channel, never resolve a tool-bearing task
    // to reasoning-off — that is the self-inflicted billed-noncommittal
    // failure. Floor to the lowest supported effort (or "low") so no future
    // auto default, capability override, or session pin can re-introduce
    // it. Explicit non-auto caller policies are honored above; this guard
    // only rewrites a resolved "off" for the tool tasks.
    if caps.reasoning_required_for_tools
        && level == "off"
        && matches!(task.as_str(), "agent" | "code" | "verify")
    {
        level = lowest_tool_reasoning_level(caps);
    }
    let Some((thinking, effective_level)) = thinking_for_reasoning_level(&level, caps) else {
        return Ok(None);
    };
    Ok(Some(ReasoningPolicyApplication {
        thinking,
        policy,
        task,
        scale,
        level: effective_level,
        provider: provider.to_string(),
        model: model.to_string(),
    }))
}

fn selected_policy(
    options: Option<&crate::value::DictMap>,
    default_policy: Option<&str>,
) -> Result<Option<String>, VmError> {
    if let Some(value) = options.and_then(|opts| {
        opts.get("reasoning_policy")
            .or_else(|| opts.get("thinking_policy"))
    }) {
        return normalize_policy_vm_value(value).map(Some);
    }
    if let Some(session_id) = crate::agent_sessions::current_session_id() {
        if let Some(policy) = crate::agent_sessions::pinned_reasoning_policy(&session_id) {
            return Ok(Some(policy));
        }
    }
    default_policy.map(normalize_policy_str_vm).transpose()
}

fn caller_set_reasoning(options: Option<&crate::value::DictMap>) -> bool {
    let Some(opts) = options else {
        return false;
    };
    if opts.contains_key("thinking") || opts.contains_key("reasoning_effort") {
        return true;
    }
    opts.get("llm_options")
        .and_then(VmValue::as_dict)
        .is_some_and(|llm_opts| {
            llm_opts.contains_key("thinking") || llm_opts.contains_key("reasoning_effort")
        })
}

fn normalize_policy_vm_value(value: &VmValue) -> Result<String, VmError> {
    match value {
        VmValue::Nil => Ok("auto".to_string()),
        VmValue::Bool(true) => Ok("auto".to_string()),
        VmValue::Bool(false) => Ok("off".to_string()),
        other => normalize_policy_str_vm(&other.display()),
    }
}

fn normalize_policy_str_vm(raw: &str) -> Result<String, VmError> {
    normalize_policy_str(raw)
        .map_err(|message| VmError::Runtime(format!("reasoning_policy: {message}")))
}

fn normalize_policy_str(raw: &str) -> Result<String, String> {
    let policy = raw.trim().to_ascii_lowercase();
    if policy.is_empty() || policy == "default" || policy == "inherit" {
        return Ok("auto".to_string());
    }
    if matches!(
        policy.as_str(),
        "none" | "disabled" | "disable" | "false" | "no" | "nothink" | "no_think"
    ) {
        return Ok("off".to_string());
    }
    if matches!(policy.as_str(), "enabled" | "on" | "true") {
        return Ok("auto".to_string());
    }
    if POLICY_VALUES.iter().any(|candidate| *candidate == policy) {
        return Ok(policy);
    }
    Err(format!(
        "expected auto, off, minimal, low, medium, high, or xhigh; got {raw:?}"
    ))
}

fn reasoning_scale(options: Option<&crate::value::DictMap>) -> Result<String, VmError> {
    let raw = options
        .and_then(|opts| {
            opts.get("reasoning_scale")
                .or_else(|| opts.get("problem_scale"))
        })
        .map(VmValue::display)
        .unwrap_or_else(|| "medium".to_string());
    let scale = raw.trim().to_ascii_lowercase();
    if scale.is_empty() || scale == "auto" {
        return Ok("medium".to_string());
    }
    if matches!(scale.as_str(), "small" | "medium" | "large") {
        return Ok(scale);
    }
    Err(VmError::Runtime(format!(
        "reasoning_scale: expected small, medium, large, or auto; got {raw:?}"
    )))
}

fn reasoning_task(options: Option<&crate::value::DictMap>) -> Result<String, VmError> {
    let raw = options.and_then(|opts| {
        opts.get("reasoning_task")
            .or_else(|| opts.get("task_kind"))
            .or_else(|| opts.get("task"))
    });
    if let Some(raw) = raw {
        let task = raw.display().trim().to_ascii_lowercase();
        if task.is_empty() {
            return Ok("chat".to_string());
        }
        if matches!(
            task.as_str(),
            "chat" | "agent" | "code" | "verify" | "summarize"
        ) {
            return Ok(task);
        }
        return Err(VmError::Runtime(format!(
            "reasoning_task: expected chat, agent, code, verify, or summarize; got {:?}",
            raw.display()
        )));
    }
    if options
        .and_then(|opts| opts.get("profile"))
        .is_some_and(|profile| profile.display() == "verifier")
    {
        return Ok("verify".to_string());
    }
    if options
        .and_then(|opts| opts.get("profile"))
        .is_some_and(|profile| profile.display() == "completer")
    {
        return Ok("summarize".to_string());
    }
    if options.and_then(|opts| opts.get("tools")).is_some() {
        return Ok("agent".to_string());
    }
    Ok("chat".to_string())
}

fn auto_reasoning_level(task: &str, scale: &str) -> String {
    if task == "summarize" {
        return "off".to_string();
    }
    if task == "verify" {
        return "low".to_string();
    }
    if task == "chat" && scale != "large" {
        return "off".to_string();
    }
    match scale {
        "small" => "low".to_string(),
        "large" => "high".to_string(),
        _ => "medium".to_string(),
    }
}

fn thinking_for_reasoning_level(
    level: &str,
    caps: &Capabilities,
) -> Option<(ThinkingConfig, String)> {
    if level == "off" {
        if caps_supports(caps, "effort") || caps.reasoning_effort_supported {
            if caps.reasoning_none_supported {
                return Some((
                    ThinkingConfig::Effort {
                        level: ReasoningEffort::None,
                    },
                    "none".to_string(),
                ));
            }
            let level = lowest_supported_effort(caps).unwrap_or(ReasoningEffort::Minimal);
            return Some((ThinkingConfig::Effort { level }, level.as_str().to_string()));
        }
        return Some((ThinkingConfig::Disabled, "off".to_string()));
    }
    if caps_supports(caps, "effort") || caps.reasoning_effort_supported {
        return reasoning_effort_from_level(level)
            .map(|level| (ThinkingConfig::Effort { level }, level.as_str().to_string()));
    }
    if caps_supports(caps, "enabled") {
        return Some((
            ThinkingConfig::Enabled {
                budget_tokens: Some(budget_for_reasoning_level(level)),
            },
            level.to_string(),
        ));
    }
    if caps_supports(caps, "adaptive") {
        return Some((ThinkingConfig::Adaptive, level.to_string()));
    }
    None
}

fn caps_supports(caps: &Capabilities, mode: &str) -> bool {
    caps.thinking_modes
        .iter()
        .any(|supported| supported == mode)
}

fn reasoning_effort_from_level(level: &str) -> Option<ReasoningEffort> {
    Some(match level {
        "minimal" => ReasoningEffort::Minimal,
        "low" => ReasoningEffort::Low,
        "medium" => ReasoningEffort::Medium,
        "high" => ReasoningEffort::High,
        "xhigh" => ReasoningEffort::XHigh,
        _ => return None,
    })
}

fn lowest_supported_effort(caps: &Capabilities) -> Option<ReasoningEffort> {
    ["minimal", "low", "medium", "high", "xhigh"]
        .into_iter()
        .find_map(|candidate| {
            caps.reasoning_effort_levels
                .iter()
                .any(|supported| supported == candidate)
                .then(|| reasoning_effort_from_level(candidate))
                .flatten()
        })
}

/// Lowest reasoning level to use when a route requires reasoning for tool
/// calls (`reasoning_required_for_tools`) but the policy resolved to "off".
/// Prefer the route's lowest declared `reasoning_effort_levels` entry (so
/// Cerebras gpt-oss, which rejects "minimal"/"none", floors to its accepted
/// "low"); fall back to "low" otherwise.
fn lowest_tool_reasoning_level(caps: &Capabilities) -> String {
    for candidate in ["minimal", "low", "medium", "high", "xhigh"] {
        if caps
            .reasoning_effort_levels
            .iter()
            .any(|supported| supported == candidate)
        {
            return candidate.to_string();
        }
    }
    "low".to_string()
}

/// Canonical reasoning-channel output budget (tokens) Harn's reasoning policy
/// allocates for an effort level. This is the single source of truth for the
/// effort -> token mapping; the `llm_reasoning_effort_budget` builtin and the
/// `std/llm/safe` structured-floor fallback both resolve through it rather than
/// re-hardcoding these numbers.
pub(crate) fn budget_for_reasoning_level(level: &str) -> u32 {
    match level {
        "minimal" | "low" => 1024,
        "high" | "xhigh" => 12_000,
        _ => 4096,
    }
}

fn resolved_route_from_options(opts: &crate::value::DictMap) -> Option<(String, String)> {
    let model = opts.get("model")?.display();
    if model.trim().is_empty() {
        return None;
    }
    let user_provider = opts
        .get("provider")
        .map(VmValue::display)
        .filter(|provider| {
            let provider = provider.trim();
            !provider.is_empty() && !provider.eq_ignore_ascii_case("auto")
        });
    let (resolved_model, provider_from_alias) = crate::llm_config::resolve_model(&model);
    let provider = user_provider
        .or(provider_from_alias)
        .unwrap_or_else(|| crate::llm_config::infer_provider(&resolved_model));
    Some((provider, resolved_model))
}

fn thinking_to_vm_value(thinking: &ThinkingConfig) -> VmValue {
    match thinking {
        ThinkingConfig::Disabled => VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("mode"),
            VmValue::String(arcstr::ArcStr::from("disabled")),
        )])),
        ThinkingConfig::Enabled { budget_tokens } => {
            let mut dict = crate::value::DictMap::from_iter([(
                crate::value::intern_key("mode"),
                VmValue::String(arcstr::ArcStr::from("enabled")),
            )]);
            if let Some(budget_tokens) = budget_tokens {
                dict.insert(
                    crate::value::intern_key("budget_tokens"),
                    VmValue::Int(*budget_tokens as i64),
                );
            }
            VmValue::dict(dict)
        }
        ThinkingConfig::Adaptive => VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("mode"),
            VmValue::String(arcstr::ArcStr::from("adaptive")),
        )])),
        ThinkingConfig::Effort { level } => VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("mode"),
                VmValue::String(arcstr::ArcStr::from("effort")),
            ),
            (
                crate::value::intern_key("level"),
                VmValue::String(arcstr::ArcStr::from(level.as_str())),
            ),
        ])),
    }
}

fn application_metadata_to_vm_value(application: &ReasoningPolicyApplication) -> VmValue {
    VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("policy"),
            VmValue::String(arcstr::ArcStr::from(application.policy.clone())),
        ),
        (
            crate::value::intern_key("task"),
            VmValue::String(arcstr::ArcStr::from(application.task.clone())),
        ),
        (
            crate::value::intern_key("scale"),
            VmValue::String(arcstr::ArcStr::from(application.scale.clone())),
        ),
        (
            crate::value::intern_key("level"),
            VmValue::String(arcstr::ArcStr::from(application.level.clone())),
        ),
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from(application.provider.clone())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from(application.model.clone())),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(opts: crate::value::DictMap) -> crate::value::DictMap {
        apply_policy_to_vm_options(&opts).expect("policy")
    }

    #[test]
    fn high_policy_maps_to_effort_for_openai_reasoning_models() {
        let opts = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("openai")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("gpt-5")),
            ),
            (
                crate::value::intern_key("reasoning_policy"),
                VmValue::String(arcstr::ArcStr::from("high")),
            ),
        ]);
        let out = apply(opts);
        let thinking = out
            .get("thinking")
            .and_then(VmValue::as_dict)
            .expect("thinking");
        assert_eq!(
            thinking.get("mode").map(VmValue::display).as_deref(),
            Some("effort")
        );
        assert_eq!(
            thinking.get("level").map(VmValue::display).as_deref(),
            Some("high")
        );
    }

    #[test]
    fn off_policy_floors_to_low_for_cerebras_gpt_oss() {
        let opts = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("cerebras")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("gpt-oss-120b")),
            ),
            (
                crate::value::intern_key("reasoning_policy"),
                VmValue::String(arcstr::ArcStr::from("off")),
            ),
        ]);
        let out = apply(opts);
        let thinking = out
            .get("thinking")
            .and_then(VmValue::as_dict)
            .expect("thinking");
        assert_eq!(
            thinking.get("mode").map(VmValue::display).as_deref(),
            Some("effort")
        );
        assert_eq!(
            thinking.get("level").map(VmValue::display).as_deref(),
            Some("low")
        );
        let applied = out
            .get("_agent_reasoning_policy_applied")
            .and_then(VmValue::as_dict)
            .expect("applied metadata");
        assert_eq!(
            applied.get("policy").map(VmValue::display).as_deref(),
            Some("off")
        );
        assert_eq!(
            applied.get("level").map(VmValue::display).as_deref(),
            Some("low")
        );
    }

    #[test]
    fn auto_policy_turns_local_qwen_agent_work_into_disabled_thinking() {
        // The capability matrix declares `auto_reasoning_overrides =
        // { agent = "off", verify = "off" }` on the ollama qwen3 rules
        // to neutralize the Qwen3 tool-call/thinking regression at the
        // data layer. Keep route-specific exceptions in data rather than
        // adding provider/model branches to `resolve_policy`.
        let opts = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("ollama")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("qwen3.6:35b-a3b-coding-nvfp4")),
            ),
            (
                crate::value::intern_key("reasoning_policy"),
                VmValue::String(arcstr::ArcStr::from("auto")),
            ),
            (
                crate::value::intern_key("reasoning_task"),
                VmValue::String(arcstr::ArcStr::from("agent")),
            ),
        ]);
        let out = apply(opts);
        let thinking = out
            .get("thinking")
            .and_then(VmValue::as_dict)
            .expect("thinking");
        assert_eq!(
            thinking.get("mode").map(VmValue::display).as_deref(),
            Some("disabled")
        );
        let applied = out
            .get("_agent_reasoning_policy_applied")
            .and_then(VmValue::as_dict)
            .expect("applied metadata");
        assert_eq!(
            applied.get("level").map(VmValue::display).as_deref(),
            Some("off")
        );
    }

    #[test]
    fn auto_policy_turns_cloud_qwen3_tool_use_into_disabled_thinking() {
        // The Qwen3 tool-call regression is in the model weights, not
        // the provider — cloud routes must downgrade too. Declared via
        // `auto_reasoning_overrides = { agent = "off" }` on the
        // openrouter qwen rules (matching qwen/qwen3.6-35b-a3b — the
        // exact model whose 5+ minute single-turn finalize bug
        // motivated this work).
        let opts = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("openrouter")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("qwen/qwen3.6-35b-a3b")),
            ),
            (
                crate::value::intern_key("reasoning_policy"),
                VmValue::String(arcstr::ArcStr::from("auto")),
            ),
            (
                crate::value::intern_key("reasoning_task"),
                VmValue::String(arcstr::ArcStr::from("agent")),
            ),
        ]);
        let out = apply(opts);
        let thinking = out
            .get("thinking")
            .and_then(VmValue::as_dict)
            .expect("thinking");
        assert_eq!(
            thinking.get("mode").map(VmValue::display).as_deref(),
            Some("disabled")
        );
    }

    #[test]
    fn explicit_high_policy_overrides_capability_matrix_override() {
        // `auto_reasoning_overrides` only fires when the resolved policy
        // is `auto`. A caller who explicitly asks for `high` reasoning
        // is acknowledged regardless of the per-route default — the
        // override declares the auto default, not a ceiling.
        let opts = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("openrouter")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("qwen/qwen3.6-35b-a3b")),
            ),
            (
                crate::value::intern_key("reasoning_policy"),
                VmValue::String(arcstr::ArcStr::from("high")),
            ),
            (
                crate::value::intern_key("reasoning_task"),
                VmValue::String(arcstr::ArcStr::from("agent")),
            ),
        ]);
        let out = apply(opts);
        let thinking = out
            .get("thinking")
            .and_then(VmValue::as_dict)
            .expect("thinking");
        // openrouter Qwen caps don't expose effort (effort is silently
        // dropped by the provider for Qwen); the resolver therefore
        // materializes "high" as Enabled{budget_tokens} with the high
        // budget — wire format `{reasoning: {max_tokens: 12000}}`.
        assert_eq!(
            thinking.get("mode").map(VmValue::display).as_deref(),
            Some("enabled")
        );
    }

    #[test]
    fn auto_reasoning_overrides_via_user_capability_toml_round_trips() {
        // Project-level overrides should be able to declare the same
        // shape (e.g. an org pinning a private model's auto behavior
        // without patching the engine).
        crate::llm::capabilities::set_user_overrides_toml(
            r#"
[[provider.acme]]
model_match = "custom-thinker"
thinking_modes = ["enabled"]
auto_reasoning_overrides = { agent = "off" }
"#,
        )
        .expect("override toml");
        let opts = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("acme")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("custom-thinker")),
            ),
            (
                crate::value::intern_key("reasoning_policy"),
                VmValue::String(arcstr::ArcStr::from("auto")),
            ),
            (
                crate::value::intern_key("reasoning_task"),
                VmValue::String(arcstr::ArcStr::from("agent")),
            ),
        ]);
        let out = apply(opts);
        let thinking = out
            .get("thinking")
            .and_then(VmValue::as_dict)
            .expect("thinking");
        assert_eq!(
            thinking.get("mode").map(VmValue::display).as_deref(),
            Some("disabled")
        );
        crate::llm::capabilities::clear_user_overrides();
    }

    fn agent_opts(provider: &str, model: &str, task: &str, policy: &str) -> crate::value::DictMap {
        crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from(provider)),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from(model)),
            ),
            (
                crate::value::intern_key("reasoning_policy"),
                VmValue::String(arcstr::ArcStr::from(policy)),
            ),
            (
                crate::value::intern_key("reasoning_task"),
                VmValue::String(arcstr::ArcStr::from(task)),
            ),
        ])
    }

    #[test]
    fn gpt_oss_tool_tasks_keep_reasoning_on_across_providers() {
        // gpt-oss performs tool calls inside the reasoning channel, so the
        // auto policy must NEVER resolve a tool-bearing task to disabled
        // reasoning. Verified for every catalogued gpt-oss route.
        for (provider, model) in [
            ("openrouter", "openai/gpt-oss-120b"),
            ("groq", "openai/gpt-oss-120b"),
            ("groq", "groq/openai/gpt-oss-120b"),
            ("cerebras", "gpt-oss-120b"),
            ("together", "openai/gpt-oss-120b"),
        ] {
            for task in ["agent", "code", "verify"] {
                let out = apply(agent_opts(provider, model, task, "auto"));
                let thinking = out
                    .get("thinking")
                    .and_then(VmValue::as_dict)
                    .unwrap_or_else(|| {
                        panic!("{provider}:{model} task={task} produced no thinking")
                    });
                let mode = thinking.get("mode").map(VmValue::display);
                assert_ne!(
                    mode.as_deref(),
                    Some("disabled"),
                    "{provider}:{model} task={task} disabled reasoning (breaks gpt-oss tool calls)"
                );
                assert_eq!(
                    mode.as_deref(),
                    Some("effort"),
                    "{provider}:{model} task={task} expected effort thinking"
                );
                // Never the reasoning-off `none` effort either.
                assert_ne!(
                    thinking.get("level").map(VmValue::display).as_deref(),
                    Some("none"),
                    "{provider}:{model} task={task} floored to reasoning-off effort"
                );
            }
        }
    }

    #[test]
    fn gpt_oss_off_pin_floors_to_low_for_tool_tasks() {
        // Even an explicit `off` pin must not disable reasoning for a
        // gpt-oss tool task — the required-for-tools guard floors it to the
        // lowest supported effort.
        let out = apply(agent_opts("cerebras", "gpt-oss-120b", "agent", "off"));
        let thinking = out
            .get("thinking")
            .and_then(VmValue::as_dict)
            .expect("thinking");
        assert_eq!(
            thinking.get("mode").map(VmValue::display).as_deref(),
            Some("effort")
        );
        assert_eq!(
            thinking.get("level").map(VmValue::display).as_deref(),
            Some("low"),
            "cerebras gpt-oss off-pin should floor to its lowest accepted effort"
        );
    }

    #[test]
    fn gpt_oss_chat_task_may_still_disable_reasoning() {
        // The guard only protects tool-bearing tasks. A plain chat turn (no
        // tools) is free to run reasoning-off — the quirk is specifically
        // about tool calls in the reasoning channel.
        let out = apply(agent_opts("cerebras", "gpt-oss-120b", "chat", "auto"));
        let thinking = out.get("thinking").and_then(VmValue::as_dict);
        // chat+auto resolves to "off"; cerebras gpt-oss floors off to its
        // lowest accepted effort ("low") via reasoning_effort_levels, but is
        // NOT forced on by the required-for-tools guard. Either way it must
        // not be the disabled-thinking mode.
        if let Some(thinking) = thinking {
            assert_ne!(
                thinking.get("mode").map(VmValue::display).as_deref(),
                Some("disabled")
            );
        }
    }

    #[test]
    fn qwen_tool_tasks_still_disable_reasoning() {
        // Regression guard: the gpt-oss fix must not change the opposite
        // Qwen3 quirk. Qwen agent tool tasks still resolve to disabled.
        for (provider, model) in [
            ("ollama", "qwen3.6:35b-a3b-coding-nvfp4"),
            ("openrouter", "qwen/qwen3.6-35b-a3b"),
        ] {
            let out = apply(agent_opts(provider, model, "agent", "auto"));
            let thinking = out
                .get("thinking")
                .and_then(VmValue::as_dict)
                .expect("thinking");
            assert_eq!(
                thinking.get("mode").map(VmValue::display).as_deref(),
                Some("disabled"),
                "{provider}:{model} qwen agent reasoning should stay disabled"
            );
        }
    }

    #[test]
    fn explicit_thinking_wins_over_policy() {
        let opts = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("openai")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("gpt-5")),
            ),
            (
                crate::value::intern_key("reasoning_policy"),
                VmValue::String(arcstr::ArcStr::from("high")),
            ),
            (crate::value::intern_key("thinking"), VmValue::Bool(true)),
        ]);
        let out = apply_policy_to_vm_options(&opts).expect("policy");
        assert!(matches!(out.get("thinking"), Some(VmValue::Bool(true))));
        assert!(!out.contains_key("_agent_reasoning_policy_applied"));
    }
}
