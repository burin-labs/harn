//! Relax a forced tool choice on routes whose catalog row rejects forcing.

thread_local! {
    static FORCED_TOOL_CHOICE_WARN_ONCE: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Lower a forced tool choice (`required` / `any` / a named tool) to `auto`
/// on routes whose catalog row says forcing is rejected (Fable 5.1, Opus 5.5:
/// `tool_choice: type "tool" and "any" are not supported for this model`).
///
/// Failing the call would turn a recoverable nudge into a dead turn, and
/// sending it is a guaranteed 400. `auto` is the provider's documented
/// replacement: the caller's prompt still asks for the tool, and loops that
/// forced a call already handle a turn without one. A
/// `disable_parallel_tool_use` flag on an object choice is kept.
pub(super) fn relax_rejected_forced_tool_choice(
    choice: serde_json::Value,
    caps: &crate::llm::capabilities::Capabilities,
    model: &str,
) -> serde_json::Value {
    if !crate::llm::providers::anthropic::tool_choice_forces_tool_use(&choice)
        || crate::llm::providers::anthropic::forced_tool_choice_allowed(caps)
    {
        return choice;
    }
    let first =
        FORCED_TOOL_CHOICE_WARN_ONCE.with(|seen| seen.borrow_mut().insert(model.to_string()));
    if first {
        crate::events::log_warn(
            "llm.tool_choice",
            &format!(
                "model \"{model}\" rejects forced tool choice; sending `tool_choice: \"auto\"` \
                 instead of {choice}"
            ),
        );
    }
    match choice.get("disable_parallel_tool_use") {
        Some(flag) => serde_json::json!({"type": "auto", "disable_parallel_tool_use": flag}),
        None => serde_json::json!("auto"),
    }
}
