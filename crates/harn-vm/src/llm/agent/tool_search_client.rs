//! Agent-loop glue for the client-executed tool-search fallback
//! (harn#70). Runs ONE `__harn_tool_search` call: executes the
//! configured strategy, emits `tool_search_query` /
//! `tool_search_result` transcript events, and promotes the matching
//! deferred tools into `opts.native_tools` so the next turn's LLM call
//! sees their full schemas.
//!
//! Matches the shape of the native Anthropic transcript events so
//! replayers and cross-provider analytics can't tell the difference
//! between the two paths (one of the explicit acceptance criteria in
//! the issue).

use std::rc::Rc;

use serde_json::Value;

use crate::bridge::HostBridge;
use crate::llm::api::{LlmCallOptions, ToolSearchStrategy, ToolSearchVariant};
use crate::llm::tool_search::{self, SearchOutcome};
use crate::value::VmError;

use super::super::helpers::transcript_event;
use super::state::{AgentLoopState, ClientToolSearchState};

/// Short reference that the `tool_search_query` transcript event uses
/// for the `name` field. Mirrors Anthropic's `tool_search_tool_{bm25,regex}`
/// naming so replays line up across providers.
pub(super) fn search_tool_display_name(variant: ToolSearchVariant) -> &'static str {
    match variant {
        ToolSearchVariant::Bm25 => "tool_search_tool_bm25",
        ToolSearchVariant::Regex => "tool_search_tool_regex",
    }
}

/// Executes a single client-mode search invocation. Returns the
/// `tool_result` payload the model should see as the tool's output.
///
/// Side effects:
///   - Appends `tool_search_query` + `tool_search_result` transcript
///     events on `state.transcript_events` (mirroring the native path
///     exactly so downstream consumers remain agnostic).
///   - Promotes matching deferred tool bodies onto `opts.native_tools`
///     for the *next* LLM call, tracking them on
///     `state.tool_search_client.promoted_*` for budget accounting.
pub(super) async fn handle_client_tool_search(
    state: &mut AgentLoopState,
    opts: &mut LlmCallOptions,
    bridge: &Option<Rc<HostBridge>>,
    tool_use_id: &str,
    raw_args: &Value,
) -> Result<String, VmError> {
    let Some(client_state) = state.tool_search_client.as_mut() else {
        return Ok(serde_json::to_string(
            &SearchOutcome::empty("internal error: client tool-search not configured")
                .into_tool_result(),
        )
        .unwrap_or_default());
    };

    let query = raw_args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Emit the `tool_search_query` event before running the strategy
    // so partial failures (host RPC error mid-flight) still record the
    // intent. Mirrors Anthropic's `server_tool_use` → `tool_search_query`
    // shape: id, name, query, visibility.
    state.transcript_events.push(transcript_event(
        "tool_search_query",
        "assistant",
        "internal",
        "",
        Some(serde_json::json!({
            "id": tool_use_id,
            "name": search_tool_display_name(client_state.variant),
            "query": raw_args.clone(),
            "strategy": client_state.strategy.as_short(),
            "mode": "client",
        })),
    ));

    let outcome = run_strategy(client_state, bridge, &query).await;
    let promoted_names = outcome.tool_names.clone();

    // Promote matching deferred tools into opts.native_tools so the
    // NEXT turn's LLM call sees their full schemas.
    let newly_added = apply_promotion(state, opts, &promoted_names);

    // Emit `tool_search_result` mirroring the Anthropic shape:
    //   { type, tool_use_id, tool_references: [{tool_name}], visibility }
    let refs: Vec<Value> = promoted_names
        .iter()
        .map(|name| serde_json::json!({"tool_name": name}))
        .collect();
    state.transcript_events.push(transcript_event(
        "tool_search_result",
        "tool",
        "internal",
        "",
        Some(serde_json::json!({
            "tool_use_id": tool_use_id,
            "tool_references": refs,
            "strategy": state
                .tool_search_client
                .as_ref()
                .map(|c| c.strategy.as_short())
                .unwrap_or("bm25"),
            "mode": "client",
            "promoted": newly_added,
        })),
    ));

    let result_value = outcome.into_tool_result();
    Ok(serde_json::to_string(&result_value).unwrap_or_default())
}

async fn run_strategy(
    state: &mut ClientToolSearchState,
    bridge: &Option<Rc<HostBridge>>,
    query: &str,
) -> SearchOutcome {
    if state.deferred_bodies.is_empty() {
        return SearchOutcome::empty(
            "no deferred tools in the search index; declare tools with \
             defer_loading: true to populate it",
        );
    }
    // Build candidates from the stashed deferred tool bodies every call.
    // Cheap — registries are small.
    let candidates_vec: Vec<Value> = state.deferred_bodies.values().cloned().collect();
    let candidates = tool_search::candidates_from_native(&candidates_vec);

    match state.strategy {
        ToolSearchStrategy::Bm25 | ToolSearchStrategy::Regex => tool_search::run_in_tree(
            state.strategy.as_in_tree(),
            query,
            &candidates,
            tool_search::DEFAULT_MAX_RESULTS,
        ),
        ToolSearchStrategy::Semantic | ToolSearchStrategy::Host => {
            // Delegate to the host via the bridge. Semantic and host
            // strategies differ only in framing — the host decides how
            // to fulfill the query. Harn preserves the returned order.
            let Some(bridge) = bridge.as_ref() else {
                return SearchOutcome::empty(
                    "tool_search strategy requires a host bridge but none is attached; \
                     run the VM via `harn run --bridge` or switch to bm25 / regex",
                );
            };
            let candidate_names: Vec<String> = candidates.iter().map(|c| c.name.clone()).collect();
            let response = bridge
                .call(
                    "tool_search/query",
                    serde_json::json!({
                        "strategy": state.strategy.as_short(),
                        "query": query,
                        "candidates": candidate_names,
                    }),
                )
                .await;
            match response {
                Ok(value) => parse_host_response(value),
                Err(err) => SearchOutcome::empty(format!(
                    "host tool_search/query failed: {err}. Fall back to \
                     bm25 or regex if this is transient."
                )),
            }
        }
    }
}

fn parse_host_response(value: Value) -> SearchOutcome {
    // Accept either `{"tool_names": [...]}` directly or an ACP-style
    // wrapper `{"result": {"tool_names": [...]}}`. Hosts tend to
    // inconsistently re-wrap — accept both.
    let target = value.get("tool_names").cloned().or_else(|| {
        value
            .get("result")
            .and_then(|r| r.get("tool_names"))
            .cloned()
    });
    let Some(Value::Array(arr)) = target else {
        return SearchOutcome::empty("host tool_search/query response missing `tool_names` array");
    };
    let names: Vec<String> = arr
        .into_iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let diagnostic = value
        .get("diagnostic")
        .and_then(|v| v.as_str())
        .map(String::from);
    SearchOutcome {
        tool_names: names,
        diagnostic,
    }
}

/// Promote the named tools onto `opts.native_tools`. Skips tools
/// already promoted (de-dup) and skips names that aren't in the
/// deferred index (host may have returned unknown names; just ignore).
/// Respects `budget_tokens` with oldest-first eviction.
///
/// Returns the names that actually got added this call (for transcript
/// telemetry).
fn apply_promotion(
    state: &mut AgentLoopState,
    opts: &mut LlmCallOptions,
    names: &[String],
) -> Vec<String> {
    let Some(client_state) = state.tool_search_client.as_mut() else {
        return Vec::new();
    };
    let native_tools = opts.native_tools.get_or_insert_with(Vec::new);

    let mut added = Vec::new();
    for name in names {
        if client_state.promoted_set.contains(name) {
            continue;
        }
        let Some(body) = client_state.deferred_bodies.get(name).cloned() else {
            continue;
        };
        let estimate = ClientToolSearchState::estimate_tokens(&body);

        // Enforce budget cap: evict oldest promotions until adding
        // `estimate` fits. `always_loaded` is outside this accounting
        // (the user pinned those tools explicitly).
        if let Some(budget) = client_state.budget_tokens {
            while client_state.current_token_total() + estimate > budget
                && !client_state.promoted_order.is_empty()
            {
                let evict = client_state.promoted_order.remove(0);
                client_state.promoted_set.remove(&evict);
                client_state.promoted_token_estimate.remove(&evict);
                native_tools.retain(|tool| {
                    let tn = tool
                        .get("name")
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            tool.get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                        })
                        .unwrap_or("");
                    tn != evict
                });
            }
            // If a single tool exceeds the budget alone, skip it and
            // record a diagnostic via the tool_names list — the caller
            // will see it in the result but next turn won't have the
            // schema. Record "no room" so telemetry makes sense.
            if estimate > budget {
                continue;
            }
        }

        native_tools.push(body);
        client_state.promoted_order.push(name.clone());
        client_state.promoted_set.insert(name.clone());
        client_state
            .promoted_token_estimate
            .insert(name.clone(), estimate);
        added.push(name.clone());
    }
    added
}
