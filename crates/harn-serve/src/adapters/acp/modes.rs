//! ACP session modes (<https://agentclientprotocol.com/protocol/session-modes>).
//!
//! A session mode is the ACP-facing name for Harn's runtime autonomy tier.
//! The catalog is fixed and is rendered both as legacy ACP `modes` and as
//! the newer `configOptions` mode selector.

use harn_vm::{orchestration::CapabilityPolicy, AutonomyTier};

/// Default mode id assigned to newly created sessions. `ask` is the
/// conservative ACP default: the agent can inspect context, but side effects
/// are held behind the approval-oriented autonomy tier until a client or user
/// explicitly switches to `code`.
pub(super) const DEFAULT_MODE_ID: &str = "ask";

pub(super) struct ModeDefinition {
    pub(super) id: &'static str,
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    autonomy_tier: AutonomyTier,
}

/// The static catalog of modes Harn advertises over ACP. Order is preserved on
/// the wire so clients render a stable selector.
pub(super) const MODE_CATALOG: &[ModeDefinition] = &[
    ModeDefinition {
        id: "ask",
        name: "Ask",
        description: "Request permission before making changes.",
        autonomy_tier: AutonomyTier::ActWithApproval,
    },
    ModeDefinition {
        id: "architect",
        name: "Architect",
        description: "Design and plan without modifying the workspace.",
        autonomy_tier: AutonomyTier::Suggest,
    },
    ModeDefinition {
        id: "code",
        name: "Code",
        description: "Read, write, execute processes, and call external services.",
        autonomy_tier: AutonomyTier::ActAuto,
    },
    ModeDefinition {
        id: "shadow",
        name: "Shadow",
        description: "Evaluate the request and emit proposals without side effects.",
        autonomy_tier: AutonomyTier::Shadow,
    },
];

pub(super) fn is_known(mode_id: &str) -> bool {
    definition(mode_id).is_some()
}

pub(super) fn known_mode_ids() -> Vec<&'static str> {
    MODE_CATALOG.iter().map(|m| m.id).collect()
}

fn definition(mode_id: &str) -> Option<&'static ModeDefinition> {
    MODE_CATALOG.iter().find(|m| m.id == mode_id)
}

/// Render the spec-shaped `SessionModeState`:
/// `{ currentModeId, availableModes: [{ id, name, description }] }`.
pub(super) fn session_mode_state(current_mode_id: &str) -> serde_json::Value {
    serde_json::json!({
        "currentModeId": current_mode_id,
        "availableModes": mode_entries("id"),
    })
}

/// Render the preferred ACP `configOptions` representation for the
/// per-session knobs Harn exposes today: session mode, pinned LLM
/// model, and a provider-aware thought level. Entries follow the `select` shape (the only `type` the
/// current spec defines, per
/// <https://agentclientprotocol.com/protocol/session-config-options>).
///
/// New knobs (temperature, permissions, …) plug in here by appending
/// another entry rather than introducing a new wire surface; ACP keeps
/// `configId` open-ended so clients can ignore unknown ids without
/// breaking.
pub(super) fn config_options_state(
    current_mode_id: &str,
    pinned_model: Option<&str>,
    pinned_reasoning_policy: Option<&str>,
    budget_value: Option<&str>,
) -> serde_json::Value {
    serde_json::json!([
        {
            "id": "mode",
            "name": "Session Mode",
            "description": "Controls Harn autonomy and side-effect policy.",
            "category": "mode",
            "type": "select",
            "currentValue": current_mode_id,
            "options": mode_entries("value"),
        },
        model_config_option(pinned_model),
        reasoning_policy_config_option(pinned_reasoning_policy),
        budget_config_option(budget_value),
    ])
}

fn mode_entries(id_key: &str) -> Vec<serde_json::Value> {
    MODE_CATALOG
        .iter()
        .map(|mode| {
            let mut entry = serde_json::Map::new();
            entry.insert(id_key.to_string(), serde_json::json!(mode.id));
            entry.insert("name".to_string(), serde_json::json!(mode.name));
            entry.insert(
                "description".to_string(),
                serde_json::json!(mode.description),
            );
            serde_json::Value::Object(entry)
        })
        .collect()
}

/// Sentinel option value rendered on the model selector when no
/// session-level pin is active. Picking it through
/// `session/set_config_option` clears any prior pin and reverts the
/// session to the ambient default (env / providers.toml).
///
/// Spec note: the ACP `ConfigOption.currentValue` field has
/// `minLength: 1`, so an empty string can't represent "unpinned".
/// `@inherit` is a stable sentinel that satisfies the schema and
/// clearly signals "fall through to the ambient default" instead of
/// being mistaken for a real model id.
pub(super) const MODEL_INHERIT_VALUE: &str = "@inherit";
pub(super) const BUDGET_INHERIT_VALUE: &str = "@inherit";
pub(super) const BUDGET_OFF_VALUE: &str = "off";

fn model_config_option(pinned_model: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": "model",
        "name": "LLM Model",
        "description": "Pinned model for subsequent prompts. llm_call invocations without an \
                        explicit `model:` option resolve to this selector. Aliases and \
                        `provider:model` selectors are both accepted; pick `@inherit` to clear \
                        the pin and revert to the ambient default.",
        "category": "model",
        "type": "select",
        "currentValue": pinned_model.unwrap_or(MODEL_INHERIT_VALUE),
        "options": model_select_options(pinned_model),
    })
}

/// Curated list of model values the spec-mandated `select` renders.
/// Anything that resolves through `harn_vm::llm_config::resolve_model_info`
/// to a registered provider is accepted by the handler — the dropdown is
/// a UI hint, not the enforcement boundary.
fn model_select_options(pinned_model: Option<&str>) -> Vec<serde_json::Value> {
    let mut entries: Vec<serde_json::Value> = Vec::new();
    entries.push(serde_json::json!({
        "value": MODEL_INHERIT_VALUE,
        "name": "Inherit ambient default",
        "description": "Clear any session-level pin and use HARN_LLM_MODEL / providers.toml.",
    }));
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for alias in harn_vm::llm_config::known_model_names() {
        let resolved = harn_vm::llm_config::resolve_model_info(&alias);
        let label = format!("{alias} ({}/{})", resolved.provider, resolved.id);
        if seen.insert(alias.clone()) {
            let description = if resolved.tier.is_empty() {
                resolved.provider.clone()
            } else {
                format!("tier: {}", resolved.tier)
            };
            entries.push(serde_json::json!({
                "value": alias,
                "name": label,
                "description": description,
            }));
        }
    }
    // The currently pinned selector may be a free-form id outside the
    // alias catalog. Surface it so the dropdown reflects the real
    // state instead of showing a stale "(none)" entry.
    if let Some(pinned) = pinned_model.filter(|value| !value.is_empty()) {
        if seen.insert(pinned.to_string()) {
            entries.push(serde_json::json!({
                "value": pinned,
                "name": pinned,
                "description": "Currently pinned (not in alias catalog).",
            }));
        }
    }
    entries
}

fn reasoning_policy_config_option(pinned_policy: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": "thought_level",
        "name": "Thought Level",
        "description": "Provider-aware reasoning policy for subsequent prompts. Harn lowers this \
                        to the route's native thinking shape (`reasoning_effort`, thinking budgets, \
                        adaptive thinking, or Qwen `/no_think`). Per-call `thinking` and \
                        `reasoning_effort` options still win; pick `@inherit` to clear the pin.",
        "category": "model",
        "type": "select",
        "currentValue": pinned_policy.unwrap_or(harn_vm::llm::reasoning_policy::INHERIT_POLICY_VALUE),
        "options": reasoning_policy_select_options(),
    })
}

fn budget_config_option(budget_value: Option<&str>) -> serde_json::Value {
    let current_value = budget_value.unwrap_or(BUDGET_INHERIT_VALUE);
    serde_json::json!({
        "id": "budget",
        "name": "Call Budget",
        "description": "Per-prompt resource ceiling installed before Harn runs the next ACP turn. \
                        Values are compact JSON objects such as \
                        {\"llm_cost_usd\":0.05,\"llm_tokens\":200000}; pick `@inherit` to use \
                        the server default or `off` to disable the session override.",
        "category": "_harn_budget",
        "type": "select",
        "currentValue": current_value,
        "options": budget_select_options(current_value),
    })
}

fn budget_select_options(current_value: &str) -> Vec<serde_json::Value> {
    let mut entries = vec![
        serde_json::json!({
            "value": BUDGET_INHERIT_VALUE,
            "name": "Inherit server default",
            "description": "Use the budget configured by the ACP server embedder.",
        }),
        serde_json::json!({
            "value": BUDGET_OFF_VALUE,
            "name": "No session budget",
            "description": "Do not install an ACP session budget for subsequent prompt turns.",
        }),
        serde_json::json!({
            "value": "{\"llm_cost_usd\":0.01}",
            "name": "$0.01 per prompt",
            "description": "Stop the prompt after roughly one cent of model spend.",
        }),
        serde_json::json!({
            "value": "{\"llm_cost_usd\":0.05,\"llm_tokens\":200000}",
            "name": "$0.05 and 200k tokens",
            "description": "Cap both model spend and input+output tokens for the prompt.",
        }),
        serde_json::json!({
            "value": "{\"llm_tokens\":50000}",
            "name": "50k tokens",
            "description": "Token-only ceiling for local or unknown-price providers.",
        }),
    ];
    if !entries
        .iter()
        .any(|entry| entry["value"].as_str() == Some(current_value))
    {
        entries.push(serde_json::json!({
            "value": current_value,
            "name": "Current custom budget",
            "description": "Session budget supplied by the client.",
        }));
    }
    entries
}

fn reasoning_policy_select_options() -> Vec<serde_json::Value> {
    let mut entries = vec![serde_json::json!({
        "value": harn_vm::llm::reasoning_policy::INHERIT_POLICY_VALUE,
        "name": "Inherit script default",
        "description": "Clear the session-level thought policy pin.",
    })];
    entries.extend(
        [
            (
                "auto",
                "Auto",
                "Let Harn choose from task, scale, provider, and model capabilities.",
            ),
            (
                "off",
                "Off",
                "Disable model thinking when possible, including Qwen no-think directives.",
            ),
            (
                "minimal",
                "Minimal",
                "Use the lowest provider-supported reasoning floor.",
            ),
            (
                "low",
                "Low",
                "Light extra reasoning for verification or small tasks.",
            ),
            (
                "medium",
                "Medium",
                "Balanced reasoning for general agent work.",
            ),
            (
                "high",
                "High",
                "More reasoning for difficult planning or code changes.",
            ),
            (
                "xhigh",
                "Extra High",
                "Maximum reasoning for routes that expose it.",
            ),
        ]
        .into_iter()
        .map(|(value, name, description)| {
            serde_json::json!({
                "value": value,
                "name": name,
                "description": description,
            })
        }),
    );
    entries
}

pub(super) fn validate_reasoning_policy_selector(raw: &str) -> Result<Option<String>, String> {
    harn_vm::llm::reasoning_policy::normalize_policy_selector(raw)
}

/// Validate a model selector for `session/set_config_option(configId="model")`.
/// Returns the normalized selector (trimmed; aliases are kept verbatim so
/// the session pin tracks the user's chosen handle) or a descriptive
/// error suitable for surfacing as `invalid_model`.
///
/// The wire surface is intentionally curated: scripts that need ad-hoc
/// selectors should pass `model:` directly to `llm_call`. Accepted forms:
///
/// - empty / whitespace → `Ok(None)` (clear pin sentinel)
/// - `provider:model` / `provider/model` where provider is in `providers.toml`
/// - an alias from `known_model_names()`
/// - a model id present in `model_catalog_entries()`
pub(super) fn validate_model_selector(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == MODEL_INHERIT_VALUE {
        return Ok(None);
    }
    if let Some((provider, _model)) = split_provider_prefix(trimmed)
        .filter(|(provider, model)| !provider.trim().is_empty() && !model.trim().is_empty())
    {
        if provider == "mock" || harn_vm::llm_config::provider_config(provider).is_some() {
            return Ok(Some(trimmed.to_string()));
        }
        return Err(format!(
            "invalid_model: provider '{provider}' is not registered. Available: {}",
            harn_vm::llm_config::provider_names().join(", ")
        ));
    }
    if harn_vm::llm_config::known_model_names()
        .iter()
        .any(|name| name == trimmed)
    {
        return Ok(Some(trimmed.to_string()));
    }
    if harn_vm::llm_config::model_catalog_entry(trimmed).is_some() {
        return Ok(Some(trimmed.to_string()));
    }
    Err(format!(
        "invalid_model: '{trimmed}' is not a known alias, catalog model id, or 'provider:model' form."
    ))
}

/// Split a selector on the first provider-form separator. Both `:` and
/// `/` are recognized because the two appear in the wild — Anthropic /
/// OpenAI route paths use `/` (e.g. `anthropic/claude-opus-4-7`), while
/// Ollama tag selectors use `:` (e.g. `ollama:llama3.2:latest`).
fn split_provider_prefix(value: &str) -> Option<(&str, &str)> {
    let position = value.find([':', '/'])?;
    let (provider, rest) = value.split_at(position);
    Some((provider, &rest[1..]))
}

/// Capability ceiling enforced while a prompt runs in this mode. Harn's
/// autonomy-tier policy remains authoritative; ACP modes only select the tier.
pub(super) fn policy_for_mode(mode_id: &str) -> Option<CapabilityPolicy> {
    let mode = definition(mode_id)?;
    if mode.autonomy_tier == AutonomyTier::ActAuto {
        // Full-access mode leaves the ambient host/runtime policy as the
        // authority. Installing a no-op ceiling would still make legacy bridge
        // fallbacks look policy-governed and block them.
        return None;
    }
    Some(harn_vm::policy_for_autonomy_tier(mode.autonomy_tier))
}

/// RAII guard that pushes a CapabilityPolicy on construction and pops it on
/// drop. Full-access `code` mode has no extra policy to push.
pub(super) struct ModePolicyGuard {
    pushed: bool,
}

impl ModePolicyGuard {
    pub(super) fn enter(mode_id: &str) -> Self {
        match policy_for_mode(mode_id) {
            Some(policy) => {
                harn_vm::orchestration::push_execution_policy(policy);
                Self { pushed: true }
            }
            None => Self { pushed: false },
        }
    }
}

impl Drop for ModePolicyGuard {
    fn drop(&mut self) {
        if self.pushed {
            harn_vm::orchestration::pop_execution_policy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_expected_modes() {
        let ids = known_mode_ids();
        assert!(ids.contains(&"ask"));
        assert!(ids.contains(&"architect"));
        assert!(ids.contains(&"code"));
        assert!(ids.contains(&"shadow"));
    }

    #[test]
    fn default_mode_matches_first_catalog_entry() {
        assert_eq!(MODE_CATALOG.first().map(|m| m.id), Some(DEFAULT_MODE_ID));
    }

    #[test]
    fn session_mode_state_contains_current_and_available() {
        let state = session_mode_state("architect");
        assert_eq!(state["currentModeId"], "architect");
        let available = state["availableModes"].as_array().expect("array");
        assert_eq!(available.len(), MODE_CATALOG.len());
        assert!(available
            .iter()
            .any(|m| m["id"] == "architect" && m["name"] == "Architect"));
    }

    #[test]
    fn config_options_state_contains_mode_selector() {
        let state = config_options_state("code", None, None, None);
        let options = state.as_array().expect("config options array");
        assert_eq!(options.len(), 4);
        assert_eq!(options[0]["id"], "mode");
        assert_eq!(options[0]["currentValue"], "code");
        assert!(options[0]["options"]
            .as_array()
            .expect("mode options")
            .iter()
            .any(|m| m["value"] == "ask"));
    }

    #[test]
    fn config_options_state_includes_model_selector_with_pin_clear_sentinel() {
        let state = config_options_state("code", None, None, None);
        let options = state.as_array().expect("config options array");
        let model_option = options
            .iter()
            .find(|entry| entry["id"] == "model")
            .expect("model config option");
        assert_eq!(model_option["category"], "model");
        assert_eq!(model_option["type"], "select");
        assert_eq!(model_option["currentValue"], MODEL_INHERIT_VALUE);
        let values: Vec<&str> = model_option["options"]
            .as_array()
            .expect("model options")
            .iter()
            .map(|entry| entry["value"].as_str().expect("value string"))
            .collect();
        assert!(
            values.contains(&MODEL_INHERIT_VALUE),
            "options must include the inherit sentinel: {values:?}"
        );
    }

    #[test]
    fn config_options_state_surfaces_free_form_pinned_model() {
        let state = config_options_state("code", Some("custom-model-not-in-catalog"), None, None);
        let model_option = state
            .as_array()
            .expect("config options array")
            .iter()
            .find(|entry| entry["id"] == "model")
            .cloned()
            .expect("model config option");
        assert_eq!(model_option["currentValue"], "custom-model-not-in-catalog");
        let has_entry = model_option["options"]
            .as_array()
            .expect("model options")
            .iter()
            .any(|entry| entry["value"] == "custom-model-not-in-catalog");
        assert!(
            has_entry,
            "free-form pinned model must appear in select options"
        );
    }

    #[test]
    fn validate_model_selector_accepts_empty_as_clear_pin() {
        assert!(validate_model_selector("").unwrap().is_none());
        assert!(validate_model_selector("   ").unwrap().is_none());
        assert!(validate_model_selector("@inherit").unwrap().is_none());
    }

    #[test]
    fn config_options_state_includes_thought_level_selector() {
        let state = config_options_state("code", None, Some("high"), None);
        let thought_option = state
            .as_array()
            .expect("config options array")
            .iter()
            .find(|entry| entry["id"] == "thought_level")
            .cloned()
            .expect("thought level config option");
        assert_eq!(thought_option["category"], "model");
        assert_eq!(thought_option["type"], "select");
        assert_eq!(thought_option["currentValue"], "high");
        let values: Vec<&str> = thought_option["options"]
            .as_array()
            .expect("thought options")
            .iter()
            .map(|entry| entry["value"].as_str().expect("value string"))
            .collect();
        assert!(values.contains(&"@inherit"));
        assert!(values.contains(&"auto"));
        assert!(values.contains(&"off"));
        assert!(values.contains(&"xhigh"));
    }

    #[test]
    fn config_options_state_includes_budget_selector_with_custom_value() {
        let custom = "{\"llm_tokens\":123}";
        let state = config_options_state("code", None, None, Some(custom));
        let budget_option = state
            .as_array()
            .expect("config options array")
            .iter()
            .find(|entry| entry["id"] == "budget")
            .cloned()
            .expect("budget config option");
        assert_eq!(budget_option["category"], "_harn_budget");
        assert_eq!(budget_option["type"], "select");
        assert_eq!(budget_option["currentValue"], custom);
        assert!(budget_option["options"]
            .as_array()
            .expect("budget options")
            .iter()
            .any(|entry| entry["value"] == custom));
    }

    #[test]
    fn validate_reasoning_policy_selector_normalizes_aliases() {
        assert!(validate_reasoning_policy_selector("").unwrap().is_none());
        assert!(validate_reasoning_policy_selector("@inherit")
            .unwrap()
            .is_none());
        assert_eq!(
            validate_reasoning_policy_selector("NO_THINK")
                .unwrap()
                .as_deref(),
            Some("off"),
        );
        assert_eq!(
            validate_reasoning_policy_selector(" high ")
                .unwrap()
                .as_deref(),
            Some("high"),
        );
        assert!(validate_reasoning_policy_selector("slow").is_err());
    }

    #[test]
    fn validate_model_selector_accepts_known_alias() {
        // `claude-sonnet-4-6` is the catalog default; any registered
        // alias works for this check.
        let resolved = validate_model_selector("claude-sonnet-4-6")
            .expect("known alias should validate")
            .expect("known alias should produce a Some(...) selector");
        assert_eq!(resolved, "claude-sonnet-4-6");
    }

    #[test]
    fn validate_model_selector_rejects_unknown_provider_form() {
        let error = validate_model_selector("nosuchprovider:nosuchmodel")
            .expect_err("unknown provider must error");
        assert!(
            error.contains("invalid_model"),
            "error should be tagged invalid_model: {error}"
        );
    }

    #[test]
    fn policy_for_code_is_none() {
        assert!(policy_for_mode("code").is_none());
    }

    #[test]
    fn policy_for_architect_clamps_to_read_only() {
        let policy = policy_for_mode("architect").expect("architect has policy");
        assert_eq!(policy.side_effect_level.as_deref(), Some("read_only"));
    }

    #[test]
    fn policy_for_ask_clamps_to_read_only() {
        let policy = policy_for_mode("ask").expect("ask has policy");
        assert_eq!(policy.side_effect_level.as_deref(), Some("read_only"));
    }

    #[test]
    fn policy_for_shadow_blocks_side_effects() {
        let policy = policy_for_mode("shadow").expect("shadow has policy");
        assert_eq!(policy.side_effect_level.as_deref(), Some("none"));
        assert_eq!(policy.recursion_limit, Some(0));
    }

    #[test]
    fn policy_for_unknown_mode_is_none() {
        assert!(policy_for_mode("not-a-real-mode").is_none());
    }

    #[test]
    fn is_known_rejects_unknown_mode() {
        assert!(is_known("ask"));
        assert!(!is_known(""));
        assert!(!is_known("plan"));
    }
}
