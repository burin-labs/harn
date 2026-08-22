//! Durable assistant-message history and orphaned native tool-use repair.

use super::*;

/// Return the visible message list for an agent session.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_messages(session_id: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_messages_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let snapshot = crate::agent_sessions::transcript(&session_id);
    let messages = snapshot
        .as_ref()
        .and_then(|v| dict_get(v, "messages"))
        .cloned()
        .unwrap_or_else(|| VmValue::List(std::sync::Arc::new(Vec::new())));
    Ok(messages)
}

pub(super) fn assistant_message_from_llm_result(llm_result: &VmValue) -> VmValue {
    let text = dict_get(llm_result, "text")
        .map(|v| v.display())
        .unwrap_or_default();
    let provider = dict_get(llm_result, "provider")
        .map(|v| v.display())
        .unwrap_or_default();
    let model = dict_get(llm_result, "model")
        .map(|v| v.display())
        .unwrap_or_default();
    // Only attach provider-native tool calls to the assistant envelope.
    // Text-mode calls remain inline in `text` and are parsed from there.
    let native_calls_value = dict_get(llm_result, "native_tool_calls")
        .cloned()
        .unwrap_or(VmValue::Nil);
    let native_calls_json = list_items(&native_calls_value)
        .iter()
        .map(vm_to_json)
        .collect::<Vec<_>>();
    let durable_blocks = durable_anthropic_blocks(llm_result, &provider, &model);
    let thinking = dict_get(llm_result, "thinking").map(|v| v.display());
    if let Some(message) = assistant_messages::text_channel_provider_surprise_message(
        llm_result,
        &provider,
        &model,
        &text,
        &native_calls_json,
        thinking.as_deref(),
    ) {
        return crate::llm::pairing_receipts::attach_assistant_facts(message, llm_result);
    }
    if native_calls_json.is_empty() {
        // gpt-oss / harmony channel-leak backstop. A native-tools model is
        // supposed to split its harmony channels at the wire: analysis ->
        // `reasoning`, commentary/tool -> `tool_calls`, final -> `content`. On
        // ~23% of gpt-oss-120b turns the provider FAILS to split and collapses
        // the analysis reasoning AND the inline tool-call JSON into a single
        // `content` blob (empty `reasoning` field, empty `tool_calls`). The
        // tagged-parser merge in `vm_build_llm_result` recovers the call into
        // the unified `tool_calls` (the `tool`-key dialect now recovers too,
        // see native_json.rs) and suppresses action-only wrapper text from the
        // public text/prose fields. Replaying that raw blob back into history
        // would waste input tokens AND re-feed the model its own private
        // chain-of-thought (incl. "game the verifier" plans) on every later
        // turn.
        //
        // For a native-tools model the canonical persisted shape is structured
        // `tool_calls` + a private `reasoning` trace + a clean `content` (this
        // is exactly what a NON-leaked gpt-oss turn produces, and what the
        // native-calls-present branch below builds). So we reconstruct that
        // shape: move the leaked blob into the private `reasoning` field (it is
        // analysis CoT, not a committed answer — clean tool-call turns carry no
        // `content`), attach the recovered call to `tool_calls`, and leave
        // `content` empty. The next request's openai-compat wire already strips
        // prior-turn `reasoning` (harn#3319), so nothing dirty is re-fed.
        //
        // Pure text-format models (`native_tools == false`, e.g. local
        // llamacpp) legitimately keep their calls inline in `content` for the
        // NEXT turn's text parser to re-read, so those keep the verbatim-text
        // path below.
        if assistant_messages::supports_native_history(llm_result, &provider, &model) {
            let recovered_calls = list_items(
                &dict_get(llm_result, "tool_calls")
                    .cloned()
                    .unwrap_or(VmValue::Nil),
            )
            .iter()
            .map(vm_to_json)
            .collect::<Vec<_>>();
            if !recovered_calls.is_empty() {
                // A call was recovered from dirty content. Keep content empty,
                // matching a clean native-tool turn; preserve only an explicit
                // wire reasoning field when the provider supplied one.
                let reasoning = thinking.as_deref().filter(|t| !t.is_empty());
                let msg = build_assistant_response_message(
                    "",
                    &[],
                    &recovered_calls,
                    reasoning,
                    &provider,
                    &model,
                );
                return crate::llm::pairing_receipts::attach_assistant_facts(
                    json_to_vm(&msg),
                    llm_result,
                );
            }
        }
        let mut msg = crate::value::DictMap::new();
        if !durable_blocks.is_empty() {
            let message = build_assistant_response_message(
                &text,
                &durable_blocks,
                &[],
                thinking.as_deref(),
                &provider,
                &model,
            );
            return crate::llm::pairing_receipts::attach_assistant_facts(
                json_to_vm(&message),
                llm_result,
            );
        }
        msg.put_str("role", "assistant");
        msg.put_str("content", text);
        return crate::llm::pairing_receipts::attach_assistant_facts(
            VmValue::dict(msg),
            llm_result,
        );
    }

    let msg = build_assistant_response_message(
        &text,
        &durable_blocks,
        &native_calls_json,
        thinking.as_deref(),
        &provider,
        &model,
    );
    crate::llm::pairing_receipts::attach_assistant_facts(json_to_vm(&msg), llm_result)
}

/// Append the assistant turn from an llm_call result to the session log.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_record_assistant(session_id: string, llm_result: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
pub(super) fn host_agent_session_record_assistant_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let llm_result = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let provider = dict_get(&llm_result, "provider")
        .map(|v| v.display())
        .unwrap_or_default();
    let model = dict_get(&llm_result, "model")
        .map(|v| v.display())
        .unwrap_or_default();
    let effective_tool_format = dict_get(&llm_result, "_effective_tool_format")
        .map(|v| v.display())
        .filter(|format| !format.trim().is_empty());
    let raw_tool_calls = dict_get(&llm_result, "tool_calls")
        .cloned()
        .unwrap_or(VmValue::Nil);
    let calls_json = list_items(&raw_tool_calls)
        .iter()
        .map(vm_to_json)
        .collect::<Vec<_>>();
    crate::agent_sessions::inject_message(
        &session_id,
        assistant_message_from_llm_result(&llm_result),
    )
    .map_err(VmError::Runtime)?;
    assistant_messages::record_dispatch_receipt(
        &session_id,
        calls_json,
        provider,
        model,
        effective_tool_format,
    );
    Ok(VmValue::Nil)
}

/// Pop the trailing assistant turn from the session transcript. Used by
/// step_judge replace mode to discard a vetoed turn before regeneration.
/// Errors if the trailing message is not an assistant turn.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_pop_last_assistant(session_id: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_pop_last_assistant_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let popped =
        crate::agent_sessions::pop_last_if_assistant(&session_id).map_err(VmError::Runtime)?;
    Ok(VmValue::Bool(popped))
}

/// True when the trailing message in the session transcript is an assistant
/// turn carrying at least one structured provider-native `tool_use`/`tool_call`
/// block (as opposed to a text-channel turn that keeps its calls inline in a
/// plain-string `content`). Used by `record_tool_results` to decide whether a
/// dispatched result must ride the provider's native tool-result role even when
/// the session is text-locked (the escalation case), staying a no-op for
/// homogeneous text-channel runs.
pub(super) fn trailing_assistant_has_native_tool_use(session_id: &str) -> bool {
    let Some(transcript) = crate::agent_sessions::transcript(session_id) else {
        return false;
    };
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(VmValue::Nil),
    );
    let Some(last) = messages.last() else {
        return false;
    };
    if dict_get(last, "role")
        .map(|v| v.display())
        .unwrap_or_default()
        != "assistant"
    {
        return false;
    }
    !assistant_tool_use_blocks(last).is_empty()
}

/// Repair the transcript invariant that every assistant `tool_use`/`tool_call`
/// block is immediately followed by a matching `tool_result` before the next
/// provider request. The agent loop calls this at every inject site that
/// DECLINES to dispatch an assistant turn's tool calls (native-format fallback
/// reject, all-blank-name drop, parse-error, no-progress nudge) and would
/// otherwise append a bare user-feedback message after an orphaned `tool_use` —
/// which Anthropic rejects with a non-retryable HTTP 400 ("tool_use ids were
/// found without tool_result blocks immediately after"), killing the run.
///
/// The synthesized tool-result carries `feedback` as its observation, so the
/// model still sees the same corrective steering it would have gotten from the
/// user message — just delivered in a provider-valid tool-result envelope that
/// keeps pairing intact.
///
/// Returns the number of orphaned blocks repaired. `0` when the trailing message
/// is not an assistant turn, carries no structured tool_use (e.g. a homogeneous
/// text-format run keeps calls inline in `content`), or every block already has
/// a paired result — so this is a strict no-op for runs that already converge.
pub(super) fn pair_orphaned_tool_use(session_id: &str, feedback: &str) -> usize {
    let Some(transcript) = crate::agent_sessions::transcript(session_id) else {
        return 0;
    };
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(VmValue::Nil),
    );
    let Some(last) = messages.last() else {
        return 0;
    };
    let role = dict_get(last, "role")
        .map(|v| v.display())
        .unwrap_or_default();
    if role != "assistant" {
        return 0;
    }
    let (provider, model) = with_session(session_id, "pair_orphaned_tool_use", |session| {
        Ok((
            session.last_provider.clone().unwrap_or_default(),
            session.last_model.clone().unwrap_or_default(),
        ))
    })
    .unwrap_or_default();
    let already_paired = paired_tool_result_ids(&messages);
    let synthetic =
        synthesize_orphan_tool_results(last, &provider, &model, feedback, &already_paired);
    let mut repaired = 0;
    for message in synthetic {
        if crate::agent_sessions::inject_message(session_id, message).is_ok() {
            repaired += 1;
        }
    }
    repaired
}

/// Synthesize a matching tool-result for each orphaned `tool_use`/`tool_call`
/// block on the trailing assistant turn, carrying `feedback` as the observation,
/// so a subsequent user-feedback inject never leaves the block unpaired. Returns
/// the number of blocks repaired (`0` = no-op: not an assistant turn, no
/// structured tool calls, or already paired). See `pair_orphaned_tool_use`.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_pair_orphaned_tool_use(session_id: string, feedback: string) -> int",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_pair_orphaned_tool_use_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let feedback = args.get(1).map(|v| v.display()).unwrap_or_default();
    let repaired = pair_orphaned_tool_use(&session_id, &feedback);
    Ok(VmValue::Int(repaired as i64))
}

const MESSAGE_HISTORY_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_AGENT_SESSION_MESSAGES_BUILTIN_DEF,
    &HOST_AGENT_SESSION_RECORD_ASSISTANT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_POP_LAST_ASSISTANT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_PAIR_ORPHANED_TOOL_USE_BUILTIN_DEF,
];

pub(super) fn register_message_history_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, MESSAGE_HISTORY_BUILTINS);
}
