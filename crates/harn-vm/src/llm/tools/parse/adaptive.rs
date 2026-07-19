//! Opt-in permissive union tool-call parser (`tool_format == "adaptive"`).
//!
//! Adaptive mode is for models whose emission dialect Harn does not yet have a
//! pinned format for. It runs every text-channel lane over the response and
//! accepts a recovered call ONLY when it maps UNAMBIGUOUSLY to exactly one
//! PRESENTED tool with schema-valid arguments — the same "unique schema match"
//! primitive dispatch already uses ([`recover_tool_name_by_unique_args`]),
//! never a guess between two viable tools.
//!
//! It is DEFAULT-OFF: no catalog route resolves to `"adaptive"`, so existing
//! routes are unaffected. It is reachable only by an explicit `tool_format`
//! request/pin. The trusted lanes (tagged, fenced-json) keep their own
//! validation; the untrusted new lane below (tool-name-as-key, which the
//! pinned parsers do not recover today) passes through the hard gate before a
//! call is credited.

use crate::llm::tools::{collect_tool_schemas, validate_tool_args};
use crate::value::VmValue;

use crate::llm::tools::name_recovery::recover_tool_name_by_unique_args;

use super::TextToolParseResult;

/// Parse a model response under the permissive Adaptive union.
///
/// Strategy: take the calls the pinned lanes already recover (tagged, then
/// fenced-json — both self-validate against the presented tools), then add any
/// GATED calls from the tool-name-as-key lane that those lanes missed. Calls
/// are deduped by `(name, arguments)`. The prose/errors/violations of the lane
/// that produced calls are preserved so the agent loop still sees narration and
/// diagnostics unchanged.
pub(crate) fn parse_adaptive_tool_calls(
    text: &str,
    tools_val: Option<&VmValue>,
) -> TextToolParseResult {
    // Primary trusted lane: the canonical tagged/heredoc grammar. It also
    // recovers the `<function=NAME>` markup and back-to-back call blocks.
    let mut result = super::parse_text_tool_calls_with_tools(text, tools_val);
    // Second trusted lane: fenced-json. Only consult it when tagged found
    // nothing, so a tagged response keeps its richer prose/canonical rendering.
    if result.calls.is_empty() {
        let fenced = super::parse_fenced_json_tool_calls(text);
        if !fenced.calls.is_empty() {
            result = fenced;
        }
    }

    // New permissive lane, fully gated: the tool-name-as-key JSON dialect
    // (`{"echo_marker": {"value": "x"}}`) that no pinned lane recovers today.
    let schemas = collect_tool_schemas(tools_val, None);
    let allowed: Vec<String> = schemas.iter().map(|s| s.name.clone()).collect();
    for (name, arguments) in parse_tool_name_as_key_candidates(text) {
        // HARD GATE: the candidate name must resolve to EXACTLY ONE presented
        // tool. A known name validates directly; an unknown name is accepted
        // only when its arguments uniquely identify one callable tool. Zero or
        // ambiguous matches are rejected (never guessed) and surfaced as a
        // violation so telemetry sees the drop.
        let resolved = if allowed.iter().any(|a| a == &name) {
            if validate_tool_args(&name, &arguments, &schemas).is_ok() {
                Some(name.clone())
            } else {
                None
            }
        } else {
            recover_tool_name_by_unique_args(&arguments, &schemas, &allowed)
        };
        let Some(resolved_name) = resolved else {
            result.violations.push(format!(
                "protocol_violation: an adaptive tool-name-as-key call `{name}` did not map \
                 unambiguously to a presented tool and was rejected."
            ));
            continue;
        };
        let candidate = serde_json::json!({
            "id": format!("tc_{}", result.calls.len()),
            "name": resolved_name,
            "arguments": arguments,
        });
        if !call_already_present(&result.calls, &candidate) {
            result.calls.push(candidate);
        }
    }

    result
}

/// True when a call with the same `name` + `arguments` is already recovered,
/// so union lanes do not double-count a call two grammars both matched.
fn call_already_present(calls: &[serde_json::Value], candidate: &serde_json::Value) -> bool {
    calls.iter().any(|existing| {
        existing.get("name") == candidate.get("name")
            && existing.get("arguments") == candidate.get("arguments")
    })
}

/// Recover tool-name-as-key candidates: a top-level JSON object whose SINGLE
/// key is a tool name and whose value is the arguments object, e.g.
/// `{"echo_marker": {"value": "MK"}}`. Returns `(name, arguments)` pairs. The
/// name is NOT validated here — the caller's hard gate owns acceptance.
///
/// Deliberately narrow: only a single-key object whose value is itself an
/// object qualifies. `{"name": ..., "arguments": ...}` (the standard fenced
/// shape) has two keys and is left to the fenced-json lane; a single-key object
/// with a scalar value is not a call.
fn parse_tool_name_as_key_candidates(text: &str) -> Vec<(String, serde_json::Value)> {
    let trimmed = super::syntax::strip_thinking_tags(text);
    let trimmed = trimmed.as_ref().trim();
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(trimmed)
    else {
        return Vec::new();
    };
    if map.len() != 1 {
        return Vec::new();
    }
    let (key, value) = map.iter().next().expect("len==1");
    if !value.is_object() {
        return Vec::new();
    }
    // Reserved envelope keys are the fenced/native shape, not a tool name.
    if matches!(
        key.as_str(),
        "name" | "arguments" | "args" | "tool" | "tool_call"
    ) {
        return Vec::new();
    }
    vec![(key.clone(), value.clone())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmValue;

    /// A single-tool registry presenting `echo_marker(value)`, built in the flat
    /// `parameters = { <param>: { ... } }` shape (wrapped as `{ tools: [...] }`)
    /// that `collect_tool_schemas` parses — mirroring the conformance probe
    /// registry.
    fn echo_marker_tools() -> VmValue {
        let value_param = VmValue::dict([
            ("type", VmValue::string("string")),
            ("description", VmValue::string("The marker value to echo.")),
        ]);
        let params = VmValue::dict([("value", value_param)]);
        let tool = VmValue::dict([
            ("name", VmValue::string("echo_marker")),
            (
                "description",
                VmValue::string("Echo the probe marker exactly."),
            ),
            ("parameters", params),
        ]);
        VmValue::dict([("tools", VmValue::List(vec![tool].into()))])
    }

    fn call_names(result: &TextToolParseResult) -> Vec<String> {
        result
            .calls
            .iter()
            .map(|c| {
                c.get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn recovers_tool_name_as_key_dialect() {
        let tools = echo_marker_tools();
        let out =
            parse_adaptive_tool_calls(r#"{"echo_marker": {"value": "MK-7Q3Z"}}"#, Some(&tools));
        assert_eq!(call_names(&out), vec!["echo_marker".to_string()]);
        assert_eq!(
            out.calls[0]
                .get("arguments")
                .and_then(|a| a.get("value"))
                .and_then(|v| v.as_str()),
            Some("MK-7Q3Z")
        );
    }

    #[test]
    fn rejects_tool_name_as_key_for_unknown_tool() {
        let tools = echo_marker_tools();
        // `delete_everything` is not presented and its args do not uniquely
        // match echo_marker's schema -> rejected, never guessed.
        let out =
            parse_adaptive_tool_calls(r#"{"delete_everything": {"path": "/"}}"#, Some(&tools));
        assert!(out.calls.is_empty(), "unknown tool must not be dispatched");
        assert!(out
            .violations
            .iter()
            .any(|v| v.contains("did not map unambiguously")));
    }

    #[test]
    fn fails_closed_without_presented_tools() {
        // No tools presented -> the gate cannot resolve any name -> no calls.
        let out = parse_adaptive_tool_calls(r#"{"echo_marker": {"value": "MK"}}"#, None);
        assert!(out.calls.is_empty());
    }

    #[test]
    fn still_parses_canonical_tagged_calls() {
        let tools = echo_marker_tools();
        let out = parse_adaptive_tool_calls(
            "<tool_call>echo_marker({ value: \"MK-7Q3Z\" })</tool_call>",
            Some(&tools),
        );
        assert_eq!(call_names(&out), vec!["echo_marker".to_string()]);
    }

    #[test]
    fn credits_back_to_back_tagged_calls() {
        let tools = echo_marker_tools();
        let out = parse_adaptive_tool_calls(
            "<tool_call>echo_marker({ value: \"a\" })</tool_call>\n\
             <tool_call>echo_marker({ value: \"b\" })</tool_call>",
            Some(&tools),
        );
        assert_eq!(out.calls.len(), 2, "both tagged calls recovered");
    }

    #[test]
    fn does_not_double_count_when_two_lanes_agree() {
        let tools = echo_marker_tools();
        // A single-key JSON object that ALSO happens to parse elsewhere must
        // not be credited twice.
        let out = parse_adaptive_tool_calls(r#"{"echo_marker": {"value": "x"}}"#, Some(&tools));
        assert_eq!(out.calls.len(), 1);
    }
}
