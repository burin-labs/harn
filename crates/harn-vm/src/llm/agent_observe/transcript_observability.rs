use super::*;

/// Write the full LLM request payload to a JSONL transcript file.
///
/// Holds a process-wide mutex around the open + write so concurrent
/// transcript emitters (parallel tests, multi-tenant agent loops on the
/// same VM) never produce a torn line. POSIX `O_APPEND` only guarantees
/// atomicity for writes ≤ `PIPE_BUF` (512 bytes on macOS), and
/// `provider_call_request` events comfortably exceed that — without
/// this lock, two simultaneous `writeln!` calls on different `File`
/// handles for the same path can interleave their bytes mid-line and
/// produce invalid JSON that downstream readers (and tests) silently
/// drop.
pub(super) fn append_llm_transcript_entry(entry: &serde_json::Value) {
    let dir = current_transcript_dir();
    append_llm_transcript_entry_to_dir(entry, dir.as_deref());
}

pub(super) fn append_llm_transcript_entry_to_dir(entry: &serde_json::Value, dir: Option<&str>) {
    let mut redacted = entry.clone();
    crate::redact::current_policy().redact_json_in_place(&mut redacted);
    forward_transcript_run_events(&redacted);
    append_llm_transcript_event_log(&redacted);
    let Some(dir) = dir else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let path = format!("{dir}/llm_transcript.jsonl");
    let Ok(line) = serde_json::to_string(&redacted) else {
        return;
    };
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
}

/// Fan transcript entries out to the run-events sink (`harn run
/// --json`). [`RunEvent::Transcript`] mirrors the raw entry; tool
/// calls and tool results carried inside the transcript stream are
/// also surfaced as their own [`RunEvent::ToolCall`] /
/// [`RunEvent::ToolResult`] variants so consumers don't have to
/// re-parse the transcript shape.
///
/// `tool_call` events are emitted once per logical call, keyed off
/// `interpreted_response` (the post-parse view that resolves the final
/// tool selection). Earlier-stage entries (`provider_call_response`)
/// still appear as `transcript` events for replay, but their
/// `tool_calls` arrays are not promoted to avoid duplicate
/// `tool_call` events for the same `call_id`.
pub(super) fn forward_transcript_run_events(entry: &serde_json::Value) {
    if !crate::run_events::sink_active() {
        return;
    }
    let kind = entry
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("transcript_event")
        .to_string();
    let agent_id = entry
        .get("agent_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    if kind == "interpreted_response" {
        if let Some(calls) = entry.get("tool_calls").and_then(|value| value.as_array()) {
            for call in calls {
                let name = call
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = call
                    .get("id")
                    .or_else(|| call.get("call_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = call
                    .get("arguments")
                    .or_else(|| call.get("args"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                crate::run_events::emit(crate::run_events::RunEvent::ToolCall {
                    call_id: id,
                    name,
                    args,
                    started_at: chrono_now(),
                });
            }
        }
    }

    if kind == "tool_result" {
        let call_id = entry
            .get("call_id")
            .or_else(|| entry.get("id"))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let ok = entry
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        let result = entry
            .get("result")
            .or_else(|| entry.get("content"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        crate::run_events::emit(crate::run_events::RunEvent::ToolResult {
            call_id,
            ok,
            result,
        });
    }

    crate::run_events::emit(crate::run_events::RunEvent::Transcript {
        agent_id,
        kind,
        payload: entry.clone(),
    });
}

pub(super) fn append_llm_transcript_event_log(entry: &serde_json::Value) {
    let Some(log) = crate::event_log::active_event_log() else {
        return;
    };
    let topic = crate::event_log::Topic::new(crate::event_log::HARN_LLM_TRANSCRIPT_TOPIC)
        .expect("static transcript topic should be valid");
    let kind = entry
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("transcript_event")
        .to_string();
    let mut headers = std::collections::BTreeMap::new();
    if let Some(span_id) = entry.get("span_id").and_then(|value| value.as_u64()) {
        headers.insert("span_id".to_string(), span_id.to_string());
    }
    if let Some(context) = crate::triggers::dispatcher::current_dispatch_context() {
        headers.insert("trigger_id".to_string(), context.binding_id.clone());
        headers.insert(
            "binding_key".to_string(),
            format!("{}@v{}", context.binding_id, context.binding_version),
        );
        headers.insert("event_id".to_string(), context.trigger_event.id.0.clone());
        headers.insert(
            "trace_id".to_string(),
            context.trigger_event.trace_id.0.clone(),
        );
        headers.insert("pipeline".to_string(), context.binding_id);
        headers.insert("action".to_string(), context.action);
        if let Some(tenant_id) = context.trigger_event.tenant_id {
            headers.insert("tenant_id".to_string(), tenant_id.0);
        }
    }
    let event = crate::event_log::LogEvent::new(kind, entry.clone()).with_headers(headers);
    // Append synchronously. Earlier this fire-and-forget `handle.spawn`ed the
    // append on the ambient tokio runtime, but the agent loop and the test
    // runner drive their runtime with `LocalSet::run_until`, which stops
    // polling once the driving future resolves. Detached append tasks were
    // therefore never polled to completion: each stranded task pinned its
    // transcript-sized `LogEvent` payload plus an `Arc<AnyEventLog>` clone for
    // the lifetime of the runtime — across an entire `harn test --parallel`
    // worker, that accumulated ~one transcript per test and OOM'd CI (#2660).
    //
    // None of the event-log backends actually yield to the tokio reactor on
    // `append` (memory = `Mutex`, sqlite = blocking `Mutex<Connection>`, file =
    // blocking fs), so a private `futures::executor::block_on` runs the append
    // to completion on this thread without touching the ambient runtime. This
    // is the same path the non-runtime branch already used.
    let _ = futures::executor::block_on(log.append(&topic, event));
}

/// Record a `template.render` transcript event for a `render()` /
/// `render_prompt()` call that resolved under an LLM-aware frame.
/// Captures the active LLM identity + capability snapshot plus the
/// branch trace produced during rendering. Replay determinism is
/// guaranteed by the renderer itself; this function is purely a
/// serializer.
pub fn record_template_render(
    template_uri: &str,
    template_revision_hash: &str,
    ctx: &crate::stdlib::template::LlmRenderContext,
    trace: &[crate::stdlib::template::BranchDecision],
    rendered_bytes: usize,
) {
    let branches = trace
        .iter()
        .map(|decision| {
            let mut entry = serde_json::Map::new();
            entry.insert(
                "kind".to_string(),
                serde_json::Value::String(decision.kind.as_str().to_string()),
            );
            entry.insert(
                "template_uri".to_string(),
                serde_json::Value::String(decision.template_uri.clone()),
            );
            entry.insert("line".to_string(), serde_json::json!(decision.line));
            entry.insert("col".to_string(), serde_json::json!(decision.col));
            entry.insert(
                "branch_id".to_string(),
                serde_json::Value::String(decision.branch_id.clone()),
            );
            if let Some(label) = decision.branch_label.as_ref() {
                entry.insert(
                    "branch_label".to_string(),
                    serde_json::Value::String(label.clone()),
                );
            }
            serde_json::Value::Object(entry)
        })
        .collect::<Vec<_>>();
    let llm = serde_json::json!({
        "provider": ctx.provider,
        "model": ctx.model,
        "family": ctx.family,
        "capabilities": vm_value_to_json(&ctx.capabilities),
    });
    let mut fields = serde_json::Map::new();
    fields.insert(
        "template_uri".to_string(),
        serde_json::Value::String(template_uri.to_string()),
    );
    fields.insert(
        "template_revision_hash".to_string(),
        serde_json::Value::String(template_revision_hash.to_string()),
    );
    fields.insert("llm".to_string(), llm);
    fields.insert("branches".to_string(), serde_json::Value::Array(branches));
    fields.insert(
        "rendered_bytes".to_string(),
        serde_json::json!(rendered_bytes),
    );
    append_llm_observability_entry("template.render", fields);
}

pub(super) fn vm_value_to_json(value: &crate::value::VmValue) -> serde_json::Value {
    use crate::value::VmValue;
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(f) => serde_json::json!(*f),
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => serde_json::Value::Object(
            d.iter()
                .map(|(k, v)| (k.to_string(), vm_value_to_json(v)))
                .collect(),
        ),
        other => serde_json::Value::String(other.display()),
    }
}

pub(crate) fn append_llm_observability_entry(
    event_type: &str,
    mut fields: serde_json::Map<String, serde_json::Value>,
) {
    fields.insert("type".to_string(), serde_json::json!(event_type));
    fields
        .entry("timestamp".to_string())
        .or_insert_with(|| serde_json::json!(chrono_now()));
    fields
        .entry("span_id".to_string())
        .or_insert_with(|| serde_json::json!(crate::tracing::current_span_id()));
    append_llm_transcript_entry(&serde_json::Value::Object(fields));
}

pub(super) fn emit_system_prompt_if_changed(system: Option<&str>) {
    let content = system.unwrap_or("");
    let current = hash_str(content);
    let content_hash = served_context_receipts::stable_redacted_string_hash(content);
    let changed = system_prompt_changed(current);
    if !changed {
        return;
    }
    append_llm_transcript_entry(&serde_json::json!({
        "type": "system_prompt",
        "timestamp": chrono_now(),
        "span_id": crate::tracing::current_span_id(),
        "hash": current,
        "content_hash": content_hash,
        "content": content,
    }));
}

pub(super) fn emit_tool_schemas_if_changed(schemas: &[crate::llm::tools::ToolSchema]) {
    let value = serde_json::to_value(schemas).unwrap_or(serde_json::Value::Null);
    let current = hash_json(&value);
    let content_hash = served_context_receipts::stable_redacted_json_hash(&value);
    let changed = tool_schemas_changed(current);
    if !changed {
        return;
    }
    append_llm_transcript_entry(&serde_json::json!({
        "type": "tool_schemas",
        "timestamp": chrono_now(),
        "span_id": crate::tracing::current_span_id(),
        "hash": current,
        "content_hash": content_hash,
        "schemas": value,
    }));
}

pub(super) fn dump_llm_request(
    iteration: usize,
    call_id: &str,
    tool_format: &str,
    opts: &super::api::LlmCallOptions,
) {
    emit_system_prompt_if_changed(opts.system.as_deref());
    let tool_schemas =
        crate::llm::tools::collect_tool_schemas(opts.tools.as_ref(), opts.native_tools.as_deref());
    emit_tool_schemas_if_changed(&tool_schemas);

    let structural_experiment = opts
        .applied_structural_experiment
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .unwrap_or(None)
        .unwrap_or(serde_json::Value::Null);
    let context_token_breakdown =
        serde_json::to_value(crate::llm::cost::project_llm_call_context_breakdown(opts))
            .unwrap_or(serde_json::Value::Null);
    emit_context_token_breakdown_checkpoint(
        opts,
        iteration,
        call_id,
        tool_format,
        &context_token_breakdown,
    );
    if let Some(decision) = opts.routing_decision.as_ref() {
        append_llm_transcript_entry(&serde_json::json!({
            "type": "routing_decision",
            "iteration": iteration,
            "call_id": call_id,
            "span_id": crate::tracing::current_span_id(),
            "timestamp": chrono_now(),
            "policy": decision.policy.clone(),
            "requested_quality": decision.requested_quality.clone(),
            "selected_provider": decision.selected_provider.clone(),
            "selected_model": decision.selected_model.clone(),
            "fallback_chain": opts.fallback_chain.clone(),
            "alternatives": decision.alternatives.clone(),
        }));
    }
    let mut request_event = serde_json::json!({
        "type": "provider_call_request",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "model": opts.model,
        "provider": opts.provider,
        "max_tokens": opts.max_tokens,
        "temperature": opts.temperature,
        "thinking": match &opts.thinking {
            super::api::ThinkingConfig::Disabled => serde_json::json!({
                "mode": "disabled",
                "enabled": false,
                "budget_tokens": serde_json::Value::Null,
            }),
            super::api::ThinkingConfig::Enabled { budget_tokens } => serde_json::json!({
                "mode": "enabled",
                "enabled": true,
                "budget_tokens": budget_tokens,
            }),
            super::api::ThinkingConfig::Adaptive => serde_json::json!({
                "mode": "adaptive",
                "enabled": true,
                "budget_tokens": serde_json::Value::Null,
            }),
            super::api::ThinkingConfig::Effort { level } => serde_json::json!({
                "mode": "effort",
                "level": level.as_str(),
                "enabled": *level != super::api::ReasoningEffort::None,
                "budget_tokens": serde_json::Value::Null,
            }),
        },
        "tool_choice": opts.tool_choice,
        "tool_format": tool_format,
        "native_tool_count": opts.native_tools.as_ref().map(|tools| tools.len()).unwrap_or(0),
        "message_count": opts.messages.len(),
        "served_context": served_context_receipts::served_context_receipt(opts, &tool_schemas),
        "context_token_breakdown": context_token_breakdown,
        "structural_experiment": structural_experiment,
        "route_policy": opts.route_policy.as_label(),
        "fallback_chain": opts.fallback_chain.clone(),
        "routing_decision": opts.routing_decision.clone(),
    });
    if verbose_llm_transcript_enabled() {
        request_event["request_snapshot"] = serde_json::json!({
            "system": opts.system,
            "messages": opts.messages,
            "tool_schemas": tool_schemas,
            "native_tools": opts.native_tools,
        });
    }
    append_llm_transcript_entry(&request_event);
}

pub(super) fn emit_context_token_breakdown_checkpoint(
    opts: &super::api::LlmCallOptions,
    iteration: usize,
    call_id: &str,
    tool_format: &str,
    context_token_breakdown: &serde_json::Value,
) {
    if !should_emit_context_token_breakdown_checkpoint(opts) {
        return;
    }
    let Some(session_id) = opts.session_id.as_deref().filter(|id| !id.is_empty()) else {
        return;
    };
    let mut checkpoint = context_token_breakdown.clone();
    let Some(object) = checkpoint.as_object_mut() else {
        return;
    };
    object.insert("call_id".to_string(), serde_json::json!(call_id));
    object.insert("iteration".to_string(), serde_json::json!(iteration));
    object.insert("provider".to_string(), serde_json::json!(opts.provider));
    object.insert("model".to_string(), serde_json::json!(opts.model));
    object.insert("tool_format".to_string(), serde_json::json!(tool_format));
    crate::llm::agent_runtime::emit_agent_event_sync(
        &crate::agent_events::AgentEvent::TypedCheckpoint {
            session_id: session_id.to_string(),
            checkpoint,
        },
    );
}

pub(super) fn should_emit_context_token_breakdown_checkpoint(
    opts: &super::api::LlmCallOptions,
) -> bool {
    opts.dispatch_provenance.is_some()
}

/// Compute the merged (native OR text-parsed) tool calls for the
/// observability response record. Mirrors the merge in
/// `crate::llm::api::result::vm_build_llm_result` (provider-native calls
/// take precedence; otherwise fall back to the calls parsed out of the
/// inline tagged `<tool_call>` blocks in `result.text`, resolved against
/// the same `tools` registry the request used so unknown-name calls are
/// not dropped). By the time the result reaches this function `text` has
/// already been canonicalized from any `[[CALL]]` wire form back to
/// `<tool_call>`, so the tagged parser sees the calls.
pub(super) fn dump_llm_response(
    iteration: usize,
    call_id: &str,
    result: &super::api::LlmResult,
    response_ms: u64,
    structural_experiment: Option<&crate::llm::structural_experiments::AppliedStructuralExperiment>,
    tools: Option<&crate::value::VmValue>,
) {
    let structural_experiment = structural_experiment
        .map(serde_json::to_value)
        .transpose()
        .unwrap_or(None)
        .unwrap_or(serde_json::Value::Null);
    let telemetry = serde_json::to_value(&result.telemetry).unwrap_or(serde_json::Value::Null);
    let parsed_tool_calls = raw_tool_receipts::merged_tool_calls_for_observability(result, tools);
    let loop_state = decode_loop_state(&result.text);
    let mut event = serde_json::json!({
        "type": "provider_call_response",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "provider": result.provider,
        "model": result.model,
        "text": result.text,
        "tool_calls": result.tool_calls,
        // Observability-only merged view: provider-native calls when present,
        // otherwise the calls parsed out of the inline tagged `<tool_call>`
        // blocks in `text`. Text-format local models (llamacpp/qwen3.6) carry
        // their calls only inline, so `tool_calls` (native) is empty for them;
        // this sidecar makes the response record self-describing. Distinct from
        // `tool_calls` so consumers can tell native vs. text-parsed apart. This
        // does NOT touch the request-construction / history path — the model's
        // next-turn payload is unchanged.
        "parsed_tool_calls": parsed_tool_calls,
        // Some agent prompts emit a machine-readable LOOP_STATE block in the
        // response text. Decode it once at Harn's transcript boundary so
        // observability consumers do not each maintain their own text parser.
        "loop_state": loop_state,
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "cost_usd": result.priced_cost_usd(),
        "cache_read_tokens": result.cache_read_tokens,
        "cache_write_tokens": result.cache_write_tokens,
        "cache_hit_ratio": crate::llm::cost::cache_hit_ratio(
            result.input_tokens,
            result.cache_read_tokens,
            result.cache_write_tokens,
        ),
        "cache_savings_usd": crate::llm::cost::cache_savings_usd_for_provider(
            &result.provider,
            &result.model,
            result.input_tokens,
            result.cache_read_tokens,
            result.cache_write_tokens,
        ),
        // Explicit bool for easy cache-regression spotting in tailed logs.
        "cache_hit": result.cache_read_tokens > 0,
        "thinking": result.thinking,
        "thinking_summary": result.thinking_summary,
        // Provider-reported finish/stop reason (`stop` / `length` /
        // `tool_calls` for OpenAI-compatibles, `end_turn` / `max_tokens` /
        // `tool_use` for Anthropic, `done_reason` for Ollama). The transport
        // layer has always captured this onto `LlmResult.stop_reason`, but the
        // observability record dropped it — so transcript mining saw
        // stop_reason=None on every provider response and truncation analysis
        // (an IDE host bug report) was blind to output-cap cuts. `null` when the
        // provider reported nothing.
        "stop_reason": result.stop_reason,
        "response_ms": response_ms,
        // Server-side runtime telemetry (Ollama timings, llama.cpp prefill /
        // decode breakdown, etc.). Empty for providers that report nothing.
        "provider_telemetry": telemetry,
        "structural_experiment": structural_experiment,
    });
    raw_tool_receipts::project_onto_event(&mut event, result);
    append_llm_transcript_entry(&event);
}

pub(super) fn decode_loop_state(text: &str) -> Option<serde_json::Value> {
    let (_, remainder) = text.split_once("## LOOP_STATE")?;
    let (block, _) = remainder.split_once("## END_LOOP_STATE")?;
    let mut fields = serde_json::Map::new();
    for line in block.lines() {
        let Some((key, raw_value)) = line.trim().split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        fields.insert(key.to_string(), loop_state_value(raw_value.trim()));
    }
    (!fields.is_empty()).then_some(serde_json::Value::Object(fields))
}

fn loop_state_value(value: &str) -> serde_json::Value {
    match value {
        "true" => serde_json::Value::Bool(true),
        "false" => serde_json::Value::Bool(false),
        "null" | "nil" => serde_json::Value::Null,
        _ => {
            if let Ok(integer) = value.parse::<i64>() {
                return serde_json::Value::from(integer);
            }
            if let Some(number) = value
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
            {
                return serde_json::Value::Number(number);
            }
            serde_json::Value::String(value.to_string())
        }
    }
}

/// Emit the self-contained `resolved_dispatch` transcript record for one LLM
/// call: the final resolved provider/model/wire_format/thinking/tool_format +
/// where each came from (`provenance`) + the normalized outcome. This is the
/// one-call answer to "what did this LLM call actually dispatch, and what did
/// it return" that used to require joining request+response events and
/// cross-referencing the capability catalog by hand.
pub(super) fn dump_resolved_dispatch(
    iteration: usize,
    call_id: &str,
    opts: &super::api::LlmCallOptions,
    effective_tool_format: &str,
    outcome: &super::resolved_dispatch::DispatchOutcome,
) {
    append_llm_transcript_entry(&super::resolved_dispatch::build_record(
        iteration,
        call_id,
        crate::tracing::current_span_id(),
        chrono_now(),
        opts,
        effective_tool_format,
        outcome,
    ));
}

pub(super) fn annotate_current_span(metadata: &[(&str, serde_json::Value)]) {
    let Some(span_id) = crate::tracing::current_span_id() else {
        return;
    };
    for (key, value) in metadata {
        crate::tracing::span_set_metadata(span_id, key, value.clone());
    }
}

pub(super) fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

/// Inputs required to wire a streaming candidate detector (harn#692)
/// into a delta-forwarding task. When supplied, the detector consumes
/// each text delta in parallel with the bridge progress notifier and
/// emits `AgentEvent::ToolCall { parsing: true, .. }` /
/// `AgentEvent::ToolCallUpdate { parsing: false, .. }` events through
/// the global session sink registry so ACP clients render an in-flight
/// chip while the model is still writing the args.
pub(crate) struct StreamingDetectorContext {
    pub session_id: String,
    pub known_tools: std::collections::BTreeSet<String>,
}

/// Create an unbounded channel and spawn a local task that forwards text
/// deltas to `bridge.send_call_progress()`. When `detector_ctx` is
/// `Some`, the same task also drives a streaming text-tool-call
/// candidate detector — emitting candidate-start / promoted / aborted
/// events via the global session sink registry as the buffer grows
/// (harn#692).
pub(super) fn spawn_progress_forwarder(
    bridge: &Arc<crate::bridge::HostBridge>,
    call_id: String,
    user_visible: bool,
    detector_ctx: Option<StreamingDetectorContext>,
    mut first_token: super::first_token::FirstTokenTimer,
) -> DeltaSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let bridge = bridge.clone();
    let mut detector = detector_ctx.map(|ctx| {
        crate::llm::tools::StreamingToolCallDetector::new(ctx.session_id, ctx.known_tools)
    });
    tokio::task::spawn_local(async move {
        let mut token_count: u64 = 0;
        while let Some(delta) = rx.recv().await {
            first_token.observe_delta();
            token_count += 1;
            bridge.send_call_progress(&call_id, &delta, token_count, user_visible);
            if let Some(d) = detector.as_mut() {
                for event in d.push(&delta) {
                    crate::agent_events::emit_event(&event);
                }
            }
        }
        if let Some(mut d) = detector {
            for event in d.finalize() {
                crate::agent_events::emit_event(&event);
            }
        }
    });
    tx
}

/// No-bridge twin of `spawn_progress_forwarder`. Drives only the
/// streaming candidate detector — the deltas are otherwise discarded
/// (the bridge progress channel is the only consumer, and we don't have
/// one). Used so non-bridge callers (offthread VM, CLI loops without an
/// attached host) still see candidate events when they have a
/// `StreamingDetectorContext`.
pub(super) fn spawn_detector_only_forwarder(
    detector_ctx: StreamingDetectorContext,
    first_token: super::first_token::FirstTokenTimer,
) -> DeltaSender {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::task::spawn_local(run_detector_loop(detector_ctx, rx, first_token));
    tx
}
