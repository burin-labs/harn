//! How a retried request differs from the attempt that failed.
//!
//! Every bounded in-call recovery in `observed_llm_call` answers the same
//! question — *what about the request changes before we try again?* — and each
//! answer is a small pure rewrite of `LlmCallOptions`. Collecting them here
//! keeps that decision in one place: a reader can see the complete set of ways
//! a retry may differ from its predecessor without reading the retry loop, and
//! a recovery that rewrites nothing is visibly a byte-identical replay.

use crate::llm::api::{LlmCallOptions, OutputFormat};

/// Rewrite a native-tool-format request onto the text channel for a retry,
/// without rebuilding the whole request from scratch. Mirrors the established
/// "text-channel request" shape (see the Ollama raw-generate test in `api.rs`):
/// drop the provider-native tool payload, force `Text` output, and clear the
/// provider-native structured output so the transport serves a plain chat completion
/// the model answers in content. The agent loop's text-tool parser then reads
/// the calls back out of the assistant text.
///
/// This is the wire-level half of the runtime tool_format fallback. It does NOT
/// re-render the system prompt's tool exemplar (that lives in the pipeline), so
/// the goal is strictly to stop a native-channel failure from hard-failing or
/// parse-looping the call — letting the model produce *parseable* output on a
/// working channel — not to guarantee identical guidance to a text-pinned run.
pub(super) fn degrade_options_to_text_channel(opts: &LlmCallOptions) -> LlmCallOptions {
    let mut degraded = opts.clone();
    degraded.native_tools = None;
    degraded.output_format = OutputFormat::Text;
    degraded
}

pub(super) fn degrade_options_to_non_streaming_transport(opts: &LlmCallOptions) -> LlmCallOptions {
    let mut degraded = opts.clone();
    degraded.stream = false;
    degraded
}

/// Raise the output cap for one retry of a call that spent its entire budget
/// and committed nothing. `None` when the cap already sits at the shared
/// escalation ceiling: another attempt there would only re-prove the same
/// exhaustion, so the call fails fast to the caller's fallback instead.
pub(super) fn escalate_options_output_budget(opts: &LlmCallOptions) -> Option<LlmCallOptions> {
    let raised = crate::llm::call::escalated_max_tokens(opts.max_tokens)?;
    let mut escalated = opts.clone();
    escalated.max_tokens = raised;
    Some(escalated)
}
