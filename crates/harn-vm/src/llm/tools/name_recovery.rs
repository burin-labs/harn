//! Recover retry guidance when a denied call lost its callable tool name but
//! retains arguments that uniquely match one dispatch-owned schema. Recovery
//! never auto-dispatches: the model must reissue the named call through the
//! normal policy and approval path.

use std::collections::BTreeSet;

use super::collect::ToolSchema;

pub(crate) fn normalize_repaired_denial(
    mut denial: crate::agent_events::ToolDenial,
    repair: Option<&serde_json::Value>,
) -> crate::agent_events::ToolDenial {
    let Some(repair) = repair else {
        return denial;
    };
    denial.gate = crate::agent_events::DenialGate::MalformedToolWrapper;
    denial.retryable = true;
    if let Some(reason) = repair.get("reason").and_then(serde_json::Value::as_str) {
        denial.reason = reason.to_string();
    }
    denial
}

/// The single tool whose required-parameter schema the arguments satisfy, or
/// `None` when zero or several tools match. Pure and schema-driven — no
/// thread-local reads — so it is exhaustively unit-testable.
///
/// A tool is a candidate when it is callable (`allowed` is empty = allow-all,
/// otherwise the name must be listed), declares at least one required parameter,
/// and every one of those parameters is present and non-null in `args`.
/// Requiring at least one *satisfied* required parameter is what makes a match
/// positive evidence: a tool with no required parameters cannot be identified
/// from arguments and never becomes a candidate, so it can never dilute
/// uniqueness.
///
/// Every supplied argument must also belong to the candidate's parameter set.
/// This prevents a partial call for one tool from being mistaken for another
/// tool whose smaller required set happens to be present. Anything ambiguous
/// or only partially covered abstains.
pub(crate) fn recover_tool_name_by_unique_args(
    args: &serde_json::Value,
    schemas: &[ToolSchema],
    allowed: &[String],
) -> Option<String> {
    let obj = args.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let is_callable =
        |name: &str| allowed.is_empty() || allowed.iter().any(|allowed| allowed == name);
    let args_match = |schema: &ToolSchema| {
        let has_required = schema
            .params
            .iter()
            .any(|param| param.required && param.default.is_none());
        let names: BTreeSet<&str> = schema
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        has_required
            && super::validate_tool_args(&schema.name, args, std::slice::from_ref(schema)).is_ok()
            && obj.keys().all(|key| names.contains(key.as_str()))
    };
    let candidates: Vec<&ToolSchema> = schemas
        .iter()
        .filter(|schema| is_callable(&schema.name) && args_match(schema))
        .collect();
    if let [only] = candidates.as_slice() {
        return Some(only.name.clone());
    }
    None
}

/// Retry-positive repair feedback for a denied call whose name is unresolvable
/// but whose arguments uniquely identify one callable tool. The dispatch-owned
/// schema set and effective policy allowlist are explicit inputs: the ambient
/// tool registry is optional language state and is not authoritative for agent
/// dispatch. Ambiguous or signal-free calls keep the unavailable-tool result.
pub(crate) fn schema_match_repair_result(
    tool_name: &str,
    tool_args: &serde_json::Value,
    schemas: &[ToolSchema],
    allowed: &[String],
) -> Option<serde_json::Value> {
    let intended = recover_tool_name_by_unique_args(tool_args, schemas, allowed)?;
    Some(build_repair_feedback(tool_name, tool_args, &intended))
}

/// Build the retry-positive `invalid_arguments` body naming the recovered tool.
/// Pure (no thread-local reads) so the feedback shape is unit-testable directly.
fn build_repair_feedback(
    tool_name: &str,
    tool_args: &serde_json::Value,
    intended: &str,
) -> serde_json::Value {
    let rendered_args = serde_json::to_string(tool_args).unwrap_or_else(|_| "{ ... }".into());
    // Echo the corrected invocation only when short enough to repeat verbatim;
    // a long payload is coached by reference so the feedback does not balloon
    // the turn (mirrors the wrapper-repair path).
    let corrected_invocation = if rendered_args.chars().count() <= 400 {
        format!("{intended}({rendered_args})")
    } else {
        format!("{intended}({{ ...the same arguments you already wrote... }})")
    };
    let reason = format!(
        "`{tool_name}` is not a callable tool name, but this call uniquely matches the \
         required arguments of `{intended}`. Some providers deliver a generic native tool-call name \
         (such as `tool_call`) when the real function name is lost in translation."
    );
    let next_step = format!(
        "Your arguments were understood but the tool name was not. Re-issue this call directly \
         with `{intended}` as the tool name: {corrected_invocation}"
    );
    serde_json::json!({
        "error": "invalid_arguments",
        "tool": tool_name,
        "recovered_tool": intended,
        "reason": reason,
        "next_step": next_step,
    })
}

#[cfg(test)]
mod tests {
    use super::super::collect::ToolSchema;
    use super::super::params::ToolParamSchema;
    use super::super::type_expr::TypeExpr;
    use super::*;

    fn param(name: &str, required: bool) -> ToolParamSchema {
        ToolParamSchema {
            name: name.to_string(),
            ty: TypeExpr::Unknown,
            description: String::new(),
            required,
            default: None,
            examples: Vec::new(),
        }
    }

    fn tool(name: &str, params: Vec<ToolParamSchema>) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: String::new(),
            params,
            compact: false,
        }
    }

    /// Representative coding tools with overlapping parameter names.
    fn coding_tools() -> Vec<ToolSchema> {
        vec![
            tool(
                "look",
                vec![
                    param("path", true),
                    param("intent", false),
                    param("line_start", false),
                    param("line_end", false),
                ],
            ),
            tool("search", vec![param("query", true), param("path", false)]),
            tool(
                "edit",
                vec![
                    param("path", true),
                    param("action", true),
                    param("content", false),
                ],
            ),
            tool("run", vec![param("command", true)]),
        ]
    }

    fn allowed() -> Vec<String> {
        ["look", "search", "edit", "run"]
            .iter()
            .map(|name| name.to_string())
            .collect()
    }

    #[test]
    fn recovers_look_from_path_only_args() {
        // No inner name or intent is available; only the required set can
        // identify the intended tool.
        let name = recover_tool_name_by_unique_args(
            &serde_json::json!({"path": "src/document.zig"}),
            &coding_tools(),
            &allowed(),
        );
        assert_eq!(name.as_deref(), Some("look"));
    }

    #[test]
    fn abstains_instead_of_misnaming_a_partial_edit_as_look() {
        // `edit` is missing its required action, while `look` does not own
        // content. Required-field matching alone would incorrectly name look.
        let name = recover_tool_name_by_unique_args(
            &serde_json::json!({"path": "src/main.zig", "content": "replacement"}),
            &coding_tools(),
            &allowed(),
        );
        assert_eq!(name, None);
    }

    #[test]
    fn recovers_run_from_command_args() {
        let name = recover_tool_name_by_unique_args(
            &serde_json::json!({"command": "zig build test"}),
            &coding_tools(),
            &allowed(),
        );
        assert_eq!(name.as_deref(), Some("run"));
    }

    #[test]
    fn recovers_edit_when_action_present_disambiguates_from_look() {
        // `{path, action}` satisfies and is fully covered by edit. Look does
        // not own `action`, so it is not a candidate.
        let name = recover_tool_name_by_unique_args(
            &serde_json::json!({"path": "a.zig", "action": "replace_range"}),
            &coding_tools(),
            &allowed(),
        );
        assert_eq!(name.as_deref(), Some("edit"));
    }

    #[test]
    fn abstains_when_no_required_schema_matches() {
        let name = recover_tool_name_by_unique_args(
            &serde_json::json!({"nonsense": 1}),
            &coding_tools(),
            &allowed(),
        );
        assert_eq!(name, None);
    }

    #[test]
    fn abstains_on_empty_args() {
        let name =
            recover_tool_name_by_unique_args(&serde_json::json!({}), &coding_tools(), &allowed());
        assert_eq!(name, None);
    }

    #[test]
    fn abstains_when_two_tools_tie_and_foreign_key_pass_cannot_break() {
        // Two tools require exactly `{path}` and both cover the argument keys:
        // genuinely ambiguous, so we must not guess.
        let schemas = vec![
            tool("look", vec![param("path", true)]),
            tool("outline", vec![param("path", true)]),
        ];
        let allowed = vec!["look".to_string(), "outline".to_string()];
        let name = recover_tool_name_by_unique_args(
            &serde_json::json!({"path": "a.zig"}),
            &schemas,
            &allowed,
        );
        assert_eq!(name, None);
    }

    #[test]
    fn never_recovers_a_tool_the_session_may_not_call() {
        // `run` uniquely matches the arguments, but it is not in the allowed
        // set — recovery must only ever name a callable tool.
        let name = recover_tool_name_by_unique_args(
            &serde_json::json!({"command": "rm -rf /"}),
            &coding_tools(),
            &["look".to_string(), "search".to_string()],
        );
        assert_eq!(name, None);
    }

    #[test]
    fn tool_with_no_required_params_never_dilutes_uniqueness() {
        // A no-required tool is vacuously "satisfied" by any args; it must be
        // excluded so it neither wins nor blocks a real unique match.
        let schemas = vec![
            tool("status", vec![param("verbose", false)]),
            tool("run", vec![param("command", true)]),
        ];
        let allowed = vec!["status".to_string(), "run".to_string()];
        let name = recover_tool_name_by_unique_args(
            &serde_json::json!({"command": "zig build"}),
            &schemas,
            &allowed,
        );
        assert_eq!(name.as_deref(), Some("run"));
    }

    #[test]
    fn repair_feedback_is_retry_positive_and_names_recovered_tool() {
        let args = serde_json::json!({"path": "src/document.zig"});
        let feedback = build_repair_feedback("tool_call", &args, "look");
        // `invalid_arguments` (not `permission_denied`) keeps the loop from
        // classifying the recovery as a hard denial the model must not retry.
        assert_eq!(feedback["error"], "invalid_arguments");
        assert_eq!(feedback["tool"], "tool_call");
        assert_eq!(feedback["recovered_tool"], "look");
        let reason = feedback["reason"].as_str().unwrap();
        assert!(
            reason.contains("`look`"),
            "reason names the recovered tool: {reason}"
        );
        let next_step = feedback["next_step"].as_str().unwrap();
        // The corrected invocation echoes the tool name and the exact arguments.
        assert!(
            next_step.contains("look({\"path\":\"src/document.zig\"})"),
            "next_step shows the direct invocation: {next_step}"
        );
    }

    #[test]
    fn repair_feedback_coaches_long_payload_by_reference() {
        // A large payload (e.g. an edit body) is not repeated verbatim.
        let big = "x".repeat(500);
        let args = serde_json::json!({"path": "a.zig", "content": big});
        let feedback = build_repair_feedback("tool_call", &args, "edit");
        let next_step = feedback["next_step"].as_str().unwrap();
        assert!(
            next_step.contains("the same arguments you already wrote"),
            "long payload coached by reference: {next_step}"
        );
        assert!(
            !next_step.contains(&"x".repeat(500)),
            "the 500-char body is not echoed"
        );
    }

    fn registry(entries: &[(&str, &[&str])]) -> crate::value::VmValue {
        let tools = entries
            .iter()
            .map(|(name, required)| {
                let parameters = required
                    .iter()
                    .map(|param| (param.to_string(), serde_json::json!({"type": "string"})))
                    .collect::<serde_json::Map<_, _>>();
                serde_json::json!({"name": name, "parameters": parameters})
            })
            .collect::<Vec<_>>();
        crate::stdlib::json_to_vm_value(&serde_json::json!({"tools": tools}))
    }

    fn dispatch_options(tools: &[&str]) -> crate::value::DictMap {
        let policy = crate::orchestration::CapabilityPolicy {
            tools: tools.iter().map(|name| (*name).to_string()).collect(),
            ..Default::default()
        };
        let mut options = crate::value::DictMap::new();
        options.insert(
            crate::value::intern_key("policy"),
            crate::stdlib::json_to_vm_value(&serde_json::to_value(policy).expect("policy JSON")),
        );
        options
    }

    fn generic_call(arguments: serde_json::Value) -> crate::value::VmValue {
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "id": "name-recovery-proof",
            "name": "tool_call",
            "arguments": arguments
        }))
    }

    async fn dispatch_generic(
        arguments: serde_json::Value,
        entries: &[(&str, &[&str])],
        allowed: &[&str],
    ) -> serde_json::Value {
        let tools = registry(entries);
        let result = crate::llm::agent_host_primitives::host_agent_dispatch_tool_call(
            crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
            generic_call(arguments),
            Some(&tools),
            &dispatch_options(allowed),
        )
        .await
        .expect("denial is a normal dispatch result");
        crate::llm::helpers::vm_value_to_json(&result)
    }

    fn assert_terminal_abstention(result: &serde_json::Value) {
        assert_eq!(
            serde_json::json!({
                "error": result["result"]["error"],
                "recovered_tool": result["result"]["recovered_tool"],
                "gate": result["denial"]["gate"],
                "retryable": result["denial"]["retryable"],
                "executor": result["executor"],
            }),
            serde_json::json!({
                "error": "unknown_tool",
                "recovered_tool": null,
                "gate": "tool_ceiling",
                "retryable": false,
                "executor": null,
            })
        );
    }

    struct OuterPolicyGuard;

    impl Drop for OuterPolicyGuard {
        fn drop(&mut self) {
            crate::orchestration::pop_execution_policy();
        }
    }

    struct ToolRegistryGuard(Option<crate::value::VmValue>);

    impl ToolRegistryGuard {
        fn install(registry: crate::value::VmValue) -> Self {
            Self(crate::stdlib::tools::install_current_tool_registry(Some(
                registry,
            )))
        }
    }

    impl Drop for ToolRegistryGuard {
        fn drop(&mut self) {
            crate::stdlib::tools::install_current_tool_registry(self.0.take());
        }
    }

    #[tokio::test]
    async fn public_dispatch_recovers_unique_name_and_restores_outer_policy() {
        let ambient_registry = registry(&[("run", &["command"])]);
        let _registry_guard = ToolRegistryGuard::install(ambient_registry.clone());
        let session_id = crate::agent_sessions::open_or_create(None);
        let outer = crate::orchestration::CapabilityPolicy {
            tools: vec!["look".into(), "edit".into()],
            workspace_roots: vec!["workspace".into()],
            ..Default::default()
        };
        crate::orchestration::push_execution_policy(outer.clone());
        let _outer_guard = OuterPolicyGuard;
        let tools = registry(&[("look", &["path"]), ("edit", &["path", "action"])]);
        let mut options = dispatch_options(&["look", "edit"]);
        options.insert(
            crate::value::intern_key("session_id"),
            crate::stdlib::json_to_vm_value(&serde_json::json!(&session_id)),
        );

        let result = crate::llm::agent_host_primitives::host_agent_dispatch_tool_call(
            crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
            generic_call(serde_json::json!({"path": "src/main.rs"})),
            Some(&tools),
            &options,
        )
        .await
        .expect("denial is a normal dispatch result");
        let result = crate::llm::helpers::vm_value_to_json(&result);

        assert_eq!(
            serde_json::json!({
                "ok": result["ok"],
                "status": result["status"],
                "tool_name": result["tool_name"],
                "error": result["result"]["error"],
                "recovered_tool": result["result"]["recovered_tool"],
                "gate": result["denial"]["gate"],
                "retryable": result["denial"]["retryable"],
                "executor": result["executor"],
            }),
            serde_json::json!({
                "ok": false,
                "status": "error",
                "tool_name": "tool_call",
                "error": "invalid_arguments",
                "recovered_tool": "look",
                "gate": "malformed_tool_wrapper",
                "retryable": true,
                "executor": null,
            })
        );
        assert_eq!(
            crate::orchestration::current_execution_policy(),
            Some(outer)
        );
        assert_eq!(
            crate::stdlib::tools::current_tool_registry()
                .as_ref()
                .map(crate::llm::helpers::vm_value_to_json),
            Some(crate::llm::helpers::vm_value_to_json(&ambient_registry))
        );
        let snapshot = crate::agent_sessions::snapshot(&session_id).expect("session snapshot");
        let snapshot = crate::llm::helpers::vm_value_to_json(&snapshot);
        let denial = snapshot["events"]
            .as_array()
            .and_then(|events| {
                events
                    .iter()
                    .find(|event| event["kind"] == "PermissionDeny")
            })
            .expect("durable PermissionDeny event");
        assert_eq!(denial["metadata"]["denial"], result["denial"]);
        assert_eq!(denial["metadata"]["reason"], result["denial"]["reason"]);
        assert_eq!(denial["text"], result["denial"]["reason"]);
        assert_eq!(denial["blocks"][0]["text"], result["denial"]["reason"]);
        assert_eq!(result["denial"]["reason"], result["result"]["reason"]);
        assert_eq!(result["error"], result["result"]["reason"]);
    }

    #[tokio::test]
    async fn public_dispatch_abstains_when_required_arguments_are_ambiguous() {
        assert_eq!(crate::orchestration::current_execution_policy(), None);
        let result = dispatch_generic(
            serde_json::json!({"path": "src/main.rs"}),
            &[("look", &["path"]), ("outline", &["path"])],
            &["look", "outline"],
        )
        .await;

        assert_terminal_abstention(&result);
        assert_eq!(crate::orchestration::current_execution_policy(), None);
    }

    #[tokio::test]
    async fn public_dispatch_abstains_when_a_partial_call_only_matches_a_smaller_schema() {
        assert_eq!(crate::orchestration::current_execution_policy(), None);
        let result = dispatch_generic(
            serde_json::json!({
                "path": "src/main.rs",
                "content": "replacement",
            }),
            &[
                ("look", &["path"]),
                ("edit", &["path", "action", "content"]),
            ],
            &["look", "edit"],
        )
        .await;

        assert_terminal_abstention(&result);
        assert_eq!(crate::orchestration::current_execution_policy(), None);
    }

    #[tokio::test]
    async fn public_dispatch_never_recovers_a_tool_outside_the_effective_ceiling() {
        assert_eq!(crate::orchestration::current_execution_policy(), None);
        let result = dispatch_generic(
            serde_json::json!({"command": "cargo test"}),
            &[("look", &["path"]), ("run", &["command"])],
            &["look"],
        )
        .await;

        assert_terminal_abstention(&result);
        assert_eq!(crate::orchestration::current_execution_policy(), None);
    }
}
