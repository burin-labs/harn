use crate::value::{VmDictExt, VmError, VmValue};

use super::CompactionPolicy;

pub(super) fn render_llm_compaction_prompt(
    summarize_prompt: Option<&str>,
    archived_messages: &[serde_json::Value],
    retained_messages: &[serde_json::Value],
    archived_count: usize,
    policy: &CompactionPolicy,
) -> Result<String, VmError> {
    let mut bindings = crate::value::DictMap::new();
    bindings.put_str(
        "formatted_messages",
        format_compaction_messages(archived_messages),
    );
    bindings.put_str(
        "retained_messages",
        format_compaction_messages(retained_messages),
    );
    bindings.insert(
        crate::value::intern_key("archived_count"),
        VmValue::Int(archived_count as i64),
    );
    bindings.insert(
        crate::value::intern_key("retained_count"),
        VmValue::Int(retained_messages.len() as i64),
    );

    let mut prompt = if policy.has_prompt_directives()
        && policy.extend_default_instructions == Some(false)
    {
        bindings.put_str("directives", policy.prompt_directives().unwrap_or_default());
        crate::stdlib::template::render_stdlib_prompt_asset(
            "orchestration/prompts/compaction_policy_replacement.harn.prompt",
            Some(&bindings),
        )?
    } else {
        let rendered = if let Some(path) = summarize_prompt.filter(|path| !path.trim().is_empty()) {
            let asset =
                crate::stdlib::template::TemplateAsset::render_target(path).map_err(|error| {
                    VmError::Runtime(format!("compaction summarize_prompt: {error}"))
                })?;
            crate::stdlib::template::render_asset_result(&asset, Some(&bindings))
                .map_err(VmError::from)?
        } else {
            crate::stdlib::template::render_stdlib_prompt_asset(
                "orchestration/prompts/compaction_summary.harn.prompt",
                Some(&bindings),
            )?
        };
        extend_compaction_prompt(rendered, policy)?
    };

    let grounding = crate::stdlib::template::render_stdlib_prompt_asset(
        "orchestration/prompts/compaction_state_grounding.harn.prompt",
        Some(&bindings),
    )?;
    prompt.push_str("\n\n");
    prompt.push_str(&grounding);
    Ok(prompt)
}

fn format_compaction_messages(messages: &[serde_json::Value]) -> String {
    messages
        .iter()
        .map(|message| {
            let role = message
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("user")
                .to_uppercase();
            let content = message
                .get("content")
                .map(format_compaction_content)
                .unwrap_or_default();
            format!("{role}: {content}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_compaction_content(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(format_compaction_content)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(map) => ["text", "content", "body", "result", "message", "error"]
            .iter()
            .filter_map(|key| map.get(*key))
            .map(format_compaction_content)
            .find(|text| !text.is_empty())
            .unwrap_or_else(|| value.to_string()),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => value.to_string(),
    }
}

fn extend_compaction_prompt(
    mut prompt: String,
    policy: &CompactionPolicy,
) -> Result<String, VmError> {
    let Some(directives) = policy.prompt_directives() else {
        return Ok(prompt);
    };
    let mut bindings = crate::value::DictMap::new();
    bindings.put_str("directives", directives);
    let extension = crate::stdlib::template::render_stdlib_prompt_asset(
        "orchestration/prompts/compaction_policy_extension.harn.prompt",
        Some(&bindings),
    )?;
    prompt.push_str("\n\n");
    prompt.push_str(&extension);
    Ok(prompt)
}
