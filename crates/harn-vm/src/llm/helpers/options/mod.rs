//! LLM call option extraction — parses the `(prompt, system, options)`
//! argument shape every high-level builtin accepts into the canonical
//! `LlmCallOptions` struct, including provider-specific warnings.
//!
//! The extractor is split by concern: [`json`] holds small value/timeout
//! parsers, [`routing`] the route-policy parser, [`output`] output-format and
//! provider-native tool shaping, [`defaults`] active-step/model-role defaults,
//! [`system_prompt`] system-prompt assembly and context fragments,
//! [`reminders`] system-reminder rendering, [`thinking`] reasoning/thinking
//! options, [`tool_search`] tool-search options, and [`extract`] the
//! `extract_llm_options` orchestrator that drives them.

mod defaults;
mod directive_placement;
mod extract;
mod generation;
mod governance;
mod json;
mod output;
mod reminders;
mod routing;
mod system_prompt;
mod thinking;
mod tool_search;
mod validate;

pub(crate) use directive_placement::uncommitted_directives;
pub(crate) use reminders::{
    apply_rendered_reminder_messages, directive_envelope_message, has_directive_commit_metadata,
    pending_reminders_from_session, render_pending_reminders,
};
#[cfg(test)]
pub(crate) use reminders::{strip_directive_commit_metadata, tracked_directive_envelope_message};
pub(crate) use validate::{project_llm_options, validate_llm_option_keys};

#[cfg(test)]
mod capability_admission_tests;
#[cfg(test)]
mod logical_defaults_tests;
#[cfg(test)]
mod output_format_tests;
#[cfg(test)]
mod reminder_render_tests;
#[cfg(test)]
mod resolve_timeout_secs_tests;
#[cfg(test)]
mod route_policy_cutover_tests;
#[cfg(test)]
mod routing_credential_tests;
#[cfg(test)]
mod routing_override_tests;
#[cfg(test)]
mod routing_responses_tests;
#[cfg(test)]
mod routing_test_support;
#[cfg(test)]
mod routing_tests;
#[cfg(test)]
mod thinking_effort_tests;

// Shared imports re-exported across the whole `options` subtree so each
// submodule only needs `use super::*;`.

pub(super) use crate::stdlib::xml::escape_xml_text;
pub(super) use crate::value::{VmError, VmValue};

pub(super) use super::{
    emit_reminder_lifecycle_event, opt_bool, opt_float, opt_int, opt_str, reminder_from_event,
    vm_messages_to_json, vm_resolve_model, vm_resolve_provider, vm_value_dict_to_json,
    vm_value_to_json, SystemReminder, REMINDER_DROPPED_EVENT_KIND, SYSTEM_REMINDER_EVENT_KIND,
};

// Public surface consumed by `super` (llm::helpers::mod).
pub(crate) use extract::extract_llm_options;
pub(crate) use generation::validate_options;
pub(crate) use governance::project_agent_tools;
pub(crate) use json::{expects_structured_output, extract_json};
pub(crate) use system_prompt::{
    assemble_system_prompt, compose_system_prompt, system_prompt_event_metadata,
    system_prompt_metadata,
};
pub(crate) use thinking::{resolve_catalog_thinking_config, resolve_thinking_config};

/// Resolve an outbound call after refreshing runtime-owned capabilities.
///
/// Ordinary throwing calls retain their established text errors. The safe
/// surface uses [`prepare_llm_options_safe`] to preserve local error taxonomy.
pub(crate) async fn prepare_llm_options(
    args: &[VmValue],
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    prepare_llm_options_result(args)
        .await
        .map_err(throwing_preflight_error)
}

/// Resolve an outbound call without discarding structured local failures.
pub(crate) async fn prepare_llm_options_safe(
    args: &[VmValue],
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    prepare_llm_options_result(args).await
}

async fn prepare_llm_options_result(
    args: &[VmValue],
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    match extract_llm_options(args) {
        Ok(initial) => {
            let (provider, model) =
                crate::llm::managed_supply::logical_route(&initial.provider, &initial.model)?;
            if crate::llm::capabilities::ensure_runtime_probe(&provider, &model).await {
                extract_llm_options(args)
            } else {
                Ok(initial)
            }
        }
        Err(initial_error) => {
            let mut options = crate::llm::cost_route::merge_context_options(
                args.get(2).and_then(VmValue::as_dict).cloned(),
            );
            defaults::apply_model_role_defaults(&mut options);
            defaults::apply_active_step_defaults(&mut options);
            let provider = vm_resolve_provider(&options);
            let model = vm_resolve_model(&options, &provider);
            let (provider, model) = crate::llm::managed_supply::logical_route(&provider, &model)?;
            if crate::llm::capabilities::ensure_runtime_probe(&provider, &model).await {
                extract_llm_options(args)
            } else {
                Err(initial_error)
            }
        }
    }
}

fn throwing_preflight_error(error: VmError) -> VmError {
    let message = match &error {
        VmError::Thrown(VmValue::Dict(fields))
            if matches!(fields.get("origin"), Some(VmValue::String(value)) if value.as_str() == "local")
                && matches!(fields.get("category"), Some(VmValue::String(value)) if value.as_str() == "invalid_request") =>
        {
            fields.get("message").and_then(|value| match value {
                VmValue::String(message) => Some(message.clone()),
                _ => None,
            })
        }
        _ => None,
    };
    message
        .map(|message| VmError::Thrown(VmValue::String(message)))
        .unwrap_or(error)
}
