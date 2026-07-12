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
mod extract;
mod json;
mod output;
mod reminders;
mod routing;
mod system_prompt;
mod thinking;
mod tool_search;

#[cfg(test)]
mod output_format_tests;
#[cfg(test)]
mod reminder_render_tests;
#[cfg(test)]
mod resolve_timeout_secs_tests;
#[cfg(test)]
mod routing_tests;

// Shared imports re-exported across the whole `options` subtree so each
// submodule only needs `use super::*;`.

pub(super) use crate::stdlib::xml::escape_xml_text;
pub(super) use crate::value::{VmError, VmValue};

pub(super) use super::{
    emit_reminder_lifecycle_event, opt_bool, opt_float, opt_int, opt_str, reminder_from_event,
    vm_messages_to_json, vm_resolve_model, vm_resolve_provider, vm_value_dict_to_json,
    vm_value_to_json, ReminderRoleHint, SystemReminder, REMINDER_DROPPED_EVENT_KIND,
    SYSTEM_REMINDER_EVENT_KIND,
};

// Public surface consumed by `super` (llm::helpers::mod).
pub(crate) use extract::extract_llm_options;
pub(crate) use json::{expects_structured_output, extract_json};
pub(crate) use system_prompt::{
    assemble_system_prompt, compose_system_prompt, system_prompt_event_metadata,
    system_prompt_metadata,
};
